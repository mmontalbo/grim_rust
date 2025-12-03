import gdb, math, os, shlex, struct

LIBLUA_PATH = __LIBLUA_PATH__
LIBLUA_NAME = os.path.basename(LIBLUA_PATH)
LUA_NEWSTATE_OFF = __LUA_NEWSTATE_OFF__
LUA_OPEN_OFF = __LUA_OPEN_OFF__
LUA_TYPE_NAMES = __LUA_TYPE_NAMES__
POINTER_HINTS = __POINTER_HINTS__
SHIM_PATH = __SHIM_PATH__
ENV_VARS = __ENV_VARS__
ENV_PRELOAD = __ENV_PRELOAD__
SLOT_SIZE = 12

def find_base(pid, needle, perms_prefix='r-x'):
    maps_path = f"/proc/{pid}/maps"
    try:
        with open(maps_path, 'r', encoding='utf-8', errors='ignore') as handle:
            for line in handle:
                if needle not in line:
                    continue
                parts = line.split()
                if len(parts) < 2:
                    continue
                perms = parts[1]
                if perms_prefix and not perms.startswith(perms_prefix):
                    continue
                rng = parts[0]
                start = rng.split('-')[0]
                try:
                    return int(start, 16)
                except ValueError:
                    continue
    except OSError as err:
        print(f"[grctl] warning: unable to read {maps_path}: {err}")
        return None
    return None

def add_symbols(pid, path, label):
    name = os.path.basename(path)
    base = find_base(pid, name)
    if base is None:
        print(f"[grctl] note: {label} not mapped; skipping add-symbol-file")
        return
    quoted = shlex.quote(path)
    try:
        gdb.execute(f"add-symbol-file {quoted} 0x{base:x}")
        print(f"[grctl] loaded symbols for {label} at 0x{base:x}")
    except gdb.error as err:
        print(f"[grctl] warning: add-symbol-file failed for {label}: {err}")

def load_symbols():
    inferior = gdb.selected_inferior()
    pid = inferior.pid if inferior else None
    if pid is None:
        print('[grctl] warning: no inferior; cannot add symbols')
        return
    add_symbols(pid, LIBLUA_PATH, 'libLua.so')
    if SHIM_PATH:
        add_symbols(pid, SHIM_PATH, 'telemetry shim')

def as_u32(val):
    try:
        return int(val.cast(gdb.lookup_type('unsigned int'))) & 0xffffffff
    except Exception:
        try:
            return int(val) & 0xffffffff
        except Exception as err:
            print(f"[grctl] warning: unexpected L value: {val} ({err})")
            return None

def read_u32(inferior, addr):
    try:
        data = inferior.read_memory(addr, 4)
        return int.from_bytes(data.tobytes(), byteorder='little', signed=False)
    except gdb.error as err:
        print(f"[grctl] warning: failed to read 0x{addr:x}: {err}")
        return None

def type_display(tt):
    name = LUA_TYPE_NAMES.get(tt)
    if name:
        return f"type={name}"
    return f"type=ttype={tt}"

def decode_value(tt, raw):
    if tt == -1:
        try:
            num_bytes = raw[4:12].tobytes()
            num = struct.unpack('<d', num_bytes)[0]
            preview = f" num={num:g}"
            if math.isnan(num) or math.isinf(num):
                bits = int.from_bytes(num_bytes, 'little', signed=False)
                preview += f" hex=0x{bits:016x}"
            return preview
        except Exception as err:
            return f" num?<err={err}>"
    if tt in POINTER_HINTS:
        ptr = int.from_bytes(raw[4:8].tobytes(), 'little', signed=False)
        label = POINTER_HINTS.get(tt)
        label_hint = f" ({label})" if label else ""
        return f" ptr=0x{ptr:x}{label_hint}"
    if tt == -6:
        return " nil"
    if tt == -11:
        line = int.from_bytes(raw[4:8].tobytes(), 'little', signed=True)
        return f" line={line}"
    return ""

def recover_stack_pointers(inferior, l_addr, window_bytes=0x400):
    try:
        data = inferior.read_memory(l_addr, window_bytes).tobytes()
    except gdb.error as err:
        print(f"[grctl] warning: unable to scan lua_State at 0x{l_addr:x}: {err}")
        return None
    words = [
        int.from_bytes(data[i:i+4], 'little', signed=False)
        for i in range(0, len(data), 4)
    ]
    candidates = []
    slot_sizes = [SLOT_SIZE, 16]
    for bi, base in enumerate(words):
        if base < 0x1000:
            continue
        for ti, top in enumerate(words):
            if top <= base or top < 0x1000:
                continue
            diff = top - base
            if diff > 0x40000:
                continue
            for slot_size in slot_sizes:
                if diff % slot_size != 0:
                    continue
                last_info = None
                for li, last in enumerate(words):
                    if last >= top and (last - base) % slot_size == 0:
                        last_info = (last, li)
                        break
                candidates.append((diff, bi, ti, last_info, base, top, slot_size))
                break
    if not candidates:
        return None
    candidates.sort(key=lambda c: c[0], reverse=True)
    diff, bi, ti, last_info, base, top, slot_size = candidates[0]
    last = last_info[0] if last_info else None
    last_off = last_info[1] * 4 if last_info else None
    return base, top, last, bi * 4, ti * 4, last_off, slot_size

def dump_lua_stack(L, max_slots=5):
    inferior = gdb.selected_inferior()
    if inferior is None:
        print('[grctl] warning: no inferior; cannot dump Lua stack')
        return
    l_addr = as_u32(L)
    if l_addr is None:
        return
    if l_addr == 0:
        print('[grctl] warning: L is null; stack unavailable')
        return
    top = read_u32(inferior, l_addr)
    base = read_u32(inferior, l_addr + 4)
    last = read_u32(inferior, l_addr + 8)
    if top is None or base is None or last is None:
        return
    offsets = (0, 4, 8)
    slot_size = SLOT_SIZE
    if base == 0 or top == 0 or top < base:
        recovered = recover_stack_pointers(inferior, l_addr)
        if recovered is None:
            try:
                data = inferior.read_memory(l_addr, 0x40).tobytes()
                words = [
                    int.from_bytes(data[i:i+4], 'little', signed=False)
                    for i in range(0, len(data), 4)
                ]
                preview = " ".join(f"0x{w:x}" for w in words)
                print(f"[grctl] lua_State@0x{l_addr:x} words: {preview}")
            except Exception:
                pass
            print(f"[grctl] lua stack unavailable (base=0x{base:x}, top=0x{top:x})")
            return
        base, top, last, base_off, top_off, last_off, slot_size = recovered
        offsets = (base_off, top_off, last_off if last_off is not None else last_off)
        print(
            "[grctl] recovered stack pointers "
            f"(base@+0x{base_off:x}, top@+0x{top_off:x}"
            + (f", last@+0x{last_off:x}" if last_off is not None else "")
            + ")"
        )
    if top < base:
        print(f"[grctl] lua stack pointers look inverted (base=0x{base:x}, top=0x{top:x})")
        return
    used_slots = (top - base) // slot_size
    capacity_slots = None
    if last >= base:
        capacity_slots = (last - base) // slot_size
    header = f"[grctl] stack base=0x{base:x} top=0x{top:x} last=0x{last:x} used={used_slots}"
    if capacity_slots is not None:
        header += f"/{capacity_slots} slots"
        if capacity_slots > used_slots:
            header += " (Lua preallocates stack capacity)"
    else:
        header += " slots"
    print(header)
    if used_slots == 0:
        print("[grctl]   (empty)")
        return

    def print_slot(idx):
        addr = base + idx * slot_size
        try:
            raw = inferior.read_memory(addr, slot_size)
        except gdb.error as err:
            print(f"[grctl] warning: unable to read slot {idx} at 0x{addr:x}: {err}")
            return False
        tt_raw = raw[0:4].tobytes()
        tt_signed = struct.unpack('<i', tt_raw)[0]
        words = [int.from_bytes(raw[i:i+4].tobytes(), 'little', signed=False) for i in range(0, slot_size, 4)]
        preview = decode_value(tt_signed, raw)
        print(f"[grctl]   [{idx}] {type_display(tt_signed)} ttype=0x{words[0]:x} v0=0x{words[1]:x} v1=0x{words[2]:x}{preview}")
        return True

    if used_slots > max_slots * 2:
        head = max_slots // 2
        tail = max_slots - head
        for idx in range(head):
            if not print_slot(idx):
                return
        omitted = used_slots - head - tail
        if omitted > 0:
            print(f"[grctl]   ... ({omitted} more slots)")
        for idx in range(used_slots - tail, used_slots):
            if idx < head:
                continue
            if not print_slot(idx):
                return
    else:
        to_show = min(used_slots, max_slots)
        for idx in range(to_show):
            if not print_slot(idx):
                return
        omitted = used_slots - to_show
        if omitted > 0:
            print(f"[grctl]   ... ({omitted} more slots)")

class LuaReturnBreakpoint(gdb.FinishBreakpoint):
    def __init__(self, frame, name):
        super().__init__(frame, internal=True)
        self.silent = False
        self.name = name

    def stop(self):
        try:
            L = getattr(self, "return_value", None)
            if L is None:
                L = gdb.parse_and_eval('$eax')
        except gdb.error as err:
            print(f"[grctl] warning: {self.name} return missing value: {err}")
            return False
        l_addr = as_u32(L)
        if l_addr is None:
            print(f"[grctl] warning: {self.name} return unreadable")
            return False
        print(f"[grctl] hit {self.name} return (L=0x{l_addr:x})")
        dump_lua_stack(L)
        return True

class LuaEntryBreakpoint(gdb.Breakpoint):
    def __init__(self, name, addr):
        super().__init__(f"*0x{addr:x}", internal=True)
        self.name = name
        self.silent = True

    def stop(self):
        frame = gdb.selected_frame()
        if frame is None:
            print(f"[grctl] warning: no frame for {self.name} entry")
            return False
        LuaReturnBreakpoint(frame, self.name)
        return False

def install_lua_breakpoint(name, addr):
    try:
        LuaEntryBreakpoint(name, addr)
        print(f"[grctl] breakpoint set at {name}=0x{addr:x}")
    except gdb.error as err:
        print(f"[grctl] warning: failed to set breakpoint for {name}: {err}")

def set_lua_alloc_breaks():
    inferior = gdb.selected_inferior()
    pid = inferior.pid if inferior else None
    if pid is None:
        print('[grctl] warning: no inferior; cannot set Lua breakpoints')
        return
    base = find_base(pid, LIBLUA_NAME)
    if base is None:
        print(f"[grctl] warning: {LIBLUA_NAME} base not found; Lua breakpoints skipped")
        return
    addrs = [
        ('lua_newstate', base + LUA_NEWSTATE_OFF),
        ('lua_open', base + LUA_OPEN_OFF),
    ]
    for name, addr in addrs:
        install_lua_breakpoint(name, addr)

def apply_env():
    for k, v in ENV_VARS.items():
        cmd = f"set environment {k} {shlex.quote(v)}"
        gdb.execute(cmd)
    gdb.execute('unset environment LD_PRELOAD')
    gdb.execute('unset environment LD_PRELOAD_32')
    if ENV_PRELOAD:
        q = shlex.quote(ENV_PRELOAD)
        gdb.execute(f"set environment LD_PRELOAD_32 {q}")
        # keep LD_PRELOAD unset to avoid preloading 32-bit shims into 64-bit gdb

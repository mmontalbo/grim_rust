COMPILER ?= zig cc
TARGET_ARCH ?= x86-linux-gnu
ARCH_FLAGS ?= -target $(TARGET_ARCH)
CFLAGS ?= -Wall -Wextra -O2 -fPIC -fno-lto
LDFLAGS ?= -ldl -pthread

TARGET := libgrim_lua_hook.so
SOURCES := lua_hook.c

.PHONY: all clean

all: $(TARGET)

$(TARGET): $(SOURCES)
	$(COMPILER) $(ARCH_FLAGS) $(CFLAGS) -shared -o $@ $^ $(LDFLAGS)

clean:
	rm -f $(TARGET)

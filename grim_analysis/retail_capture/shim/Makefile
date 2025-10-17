COMPILER ?= zig cc
TARGET_ARCH ?= x86-linux-gnu
ARCH_FLAGS ?= -target $(TARGET_ARCH)
CFLAGS ?= -Wall -Wextra -O2 -fPIC -fno-lto
LDFLAGS ?= -ldl -pthread

LUA32_PREFIX ?= $(shell command -v lua32 >/dev/null 2>&1 && dirname $$(dirname $$(command -v lua32)))
LUA32_INCLUDE ?= $(LUA32_PREFIX)/share/lua32/include

ifeq ($(strip $(LUA32_INCLUDE)),/share/lua32/include)
  $(error Unable to locate lua32 headers; set LUA32_PREFIX to the lua 3.2 installation prefix)
endif

CFLAGS += -I$(LUA32_INCLUDE)

TARGET := libgrim_lua_hook.so
SOURCES := lua_hook.c

.PHONY: all clean

all: $(TARGET)

$(TARGET): $(SOURCES)
	$(COMPILER) $(ARCH_FLAGS) $(CFLAGS) -shared -o $@ $^ $(LDFLAGS)

clean:
	rm -f $(TARGET)

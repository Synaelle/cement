#---------------------------------------------------------------------------------
# Cement 3DS server (Rust). "make" builds Cement.cia and Cement.3dsx.
#
# Needs: nightly Rust with rust-src (rustup toolchain install nightly -c rust-src),
#        cargo-3ds (cargo install --locked cargo-3ds), devkitARM with libctru
#        (DEVKITPRO and DEVKITARM set), libclang for ctru-sys's bindgen
#        (LIBCLANG_PATH if it isn't found), makerom and bannertool.
#---------------------------------------------------------------------------------

ifeq ($(strip $(DEVKITARM)),)
$(error "Please set DEVKITARM in your environment. export DEVKITARM=<path to>devkitARM")
endif

TARGET		:=	Cement
BUILD		:=	build
ELF		:=	target/armv6k-nintendo-3ds/release/cement-server.elf

# tools (override on the command line if they live elsewhere)
CARGO		?=	cargo +nightly
MAKEROM		?=	makerom
BANNERTOOL	?=	bannertool

export PATH := $(DEVKITARM)/bin:$(DEVKITPRO)/tools/bin:$(PATH)

# ctru-sys reads the gcc version from the wrong word of some devkitARM builds'
# "gcc --version", so bindgen misses the compiler headers: give it the folder
GCC_VERSION := $(shell $(DEVKITARM)/bin/arm-none-eabi-gcc -dumpversion)
export BINDGEN_EXTRA_CLANG_ARGS ?= -isystem $(DEVKITARM)/lib/gcc/arm-none-eabi/$(GCC_VERSION)/include

# version shown on the 3DS and reported to clients:
# <../VERSION>.<commit count>-<short commit>, plus -dev when there are uncommitted changes
GIT          := $(shell command -v git 2>/dev/null || (test -x "/c/Program Files/Git/cmd/git.exe" && echo "/c/Program Files/Git/cmd/git.exe"))
ifeq ($(strip $(APP_VERSION)),)
BASE_VERSION := $(strip $(shell tr -d '\r' < ../VERSION 2>/dev/null))
COMMITS      := $(strip $(shell "$(GIT)" rev-list --count HEAD 2>/dev/null))
COMMIT       := $(strip $(shell "$(GIT)" rev-parse --short HEAD 2>/dev/null))
DIRTY        := $(if $(strip $(shell "$(GIT)" status --porcelain 2>/dev/null)),-dev,)
APP_VERSION  := $(if $(COMMITS),$(BASE_VERSION).$(COMMITS)-$(COMMIT)$(DIRTY),$(BASE_VERSION)-dev)
endif

# CIA title version = build number. The HOME menu caches icons per title ID and
# title version, so it has to go up or updates keep showing the old icon.
TITLE_VERSION := $(or $(strip $(shell "$(GIT)" rev-list --count HEAD 2>/dev/null)),0)

.PHONY: all clean

all:
	@CEMENT_VERSION="$(APP_VERSION)" $(CARGO) 3ds build --release
	@mkdir -p $(BUILD)
	@$(BANNERTOOL) makebanner -i meta/banner.png -a meta/audio.wav -o $(BUILD)/banner.bnr
	@$(BANNERTOOL) makesmdh -s "Cement" -l "Fast FTP + CIA installer v$(APP_VERSION)" -p "Synaelle" -i meta/icon.png -o $(BUILD)/icon.icn
	@$(MAKEROM) -f cia -o $(TARGET).cia -target t -exefslogo -rsf meta/$(TARGET).rsf -ver $(TITLE_VERSION) \
		-elf $(ELF) -icon $(BUILD)/icon.icn -banner $(BUILD)/banner.bnr
	@cp $(ELF) $(TARGET).elf
	@echo built ... $(TARGET).cia v$(APP_VERSION)
	@3dsxtool $(TARGET).elf $(TARGET).3dsx --smdh=$(BUILD)/icon.icn
	@echo built ... $(TARGET).3dsx v$(APP_VERSION)

clean:
	@echo clean ...
	@rm -fr $(BUILD) target $(TARGET).elf $(TARGET).cia $(TARGET).3dsx

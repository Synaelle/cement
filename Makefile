#---------------------------------------------------------------------------------
# Cement 3DS server (Rust). "make" builds Cement.cia and Cement.3dsx, and
# Cement Text (the console text screens, cargo feature text-ui) as
# Cement-Text.cia and Cement-Text.3dsx. They have their own title IDs.
#
# Needs: nightly Rust with rust-src (rustup toolchain install nightly -c rust-src),
#        cargo-3ds (cargo install --locked cargo-3ds), devkitARM with libctru
#        (DEVKITPRO and DEVKITARM set), libclang for ctru-sys's bindgen
#        (LIBCLANG_PATH if it isn't found), makerom and bannertool.
#---------------------------------------------------------------------------------

ifeq ($(strip $(DEVKITARM)),)
$(error "Please set DEVKITARM in your environment. export DEVKITARM=<path to>devkitARM")
endif

BUILD		:=	build
ELF		:=	target/armv6k-nintendo-3ds/release/cement-server.elf
ELF_TEXT	:=	target/text/armv6k-nintendo-3ds/release/cement-server.elf

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
# <VERSION>.<commit count>-<short commit>, plus -dev when there are uncommitted changes
GIT          := $(shell command -v git 2>/dev/null || (test -x "/c/Program Files/Git/cmd/git.exe" && echo "/c/Program Files/Git/cmd/git.exe"))
ifeq ($(strip $(APP_VERSION)),)
BASE_VERSION := $(strip $(shell tr -d '\r' < VERSION 2>/dev/null))
COMMITS      := $(strip $(shell "$(GIT)" rev-list --count HEAD 2>/dev/null))
COMMIT       := $(strip $(shell "$(GIT)" rev-parse --short HEAD 2>/dev/null))
DIRTY        := $(if $(strip $(shell "$(GIT)" status --porcelain 2>/dev/null)),-dev,)
APP_VERSION  := $(if $(COMMITS),$(BASE_VERSION).$(COMMITS)-$(COMMIT)$(DIRTY),$(BASE_VERSION)-dev)
endif

# CIA title version = build number. The HOME menu caches icons per title ID and
# title version, so it has to go up or updates keep showing the old icon, and
# the 3DS refuses to install a lower one (D8E08027). makerom -ver only keeps 4
# bits (it wrapped every 16 builds), so the number goes in as major.minor.micro
# (10, 6 and 4 bits of one 16 bit version).
TITLE_VERSION := $(or $(strip $(shell "$(GIT)" rev-list --count HEAD 2>/dev/null)),0)
TITLE_MAJOR   := $(shell echo $$(( $(TITLE_VERSION) >> 10 )))
TITLE_MINOR   := $(shell echo $$(( ($(TITLE_VERSION) >> 4) & 63 )))
TITLE_MICRO   := $(shell echo $$(( $(TITLE_VERSION) & 15 )))

# $(call package,<file name>,<title>,<banner>,<icon>,<elf>): the .cia and the .3dsx
define package
	@mkdir -p $(BUILD)/$(1)
	@$(BANNERTOOL) makebanner -i $(3) -a meta/audio.wav -o $(BUILD)/$(1)/banner.bnr
	@$(BANNERTOOL) makesmdh -s "$(2)" -l "Fast FTP + CIA installer v$(APP_VERSION)" -p "Synaelle" -i $(4) -o $(BUILD)/$(1)/icon.icn
	@$(MAKEROM) -f cia -o $(1).cia -target t -exefslogo -rsf meta/$(1).rsf -major $(TITLE_MAJOR) -minor $(TITLE_MINOR) -micro $(TITLE_MICRO) 		-elf $(5) -icon $(BUILD)/$(1)/icon.icn -banner $(BUILD)/$(1)/banner.bnr
	@cp $(5) $(1).elf
	@echo built ... $(1).cia v$(APP_VERSION)
	@3dsxtool $(1).elf $(1).3dsx --smdh=$(BUILD)/$(1)/icon.icn
	@echo built ... $(1).3dsx v$(APP_VERSION)
endef

.PHONY: all cement cement-text clean

all: cement cement-text

cement:
	@CEMENT_VERSION="$(APP_VERSION)" $(CARGO) 3ds build --release
	$(call package,Cement,Cement,meta/banner.png,meta/icon.png,$(ELF))

cement-text:
	@CEMENT_VERSION="$(APP_VERSION)" $(CARGO) 3ds build --release --features text-ui --target-dir target/text
	$(call package,Cement-Text,Cement Text,meta/banner-text.png,meta/icon-text.png,$(ELF_TEXT))

clean:
	@echo clean ...
	@rm -fr $(BUILD) target Cement.elf Cement.cia Cement.3dsx Cement-Text.elf Cement-Text.cia Cement-Text.3dsx

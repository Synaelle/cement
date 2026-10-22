#---------------------------------------------------------------------------------
# Cement 3DS server (Rust). "make" builds Cement.cia and Cement.3dsx,
# "make PROFILE=fast" a quicker build to try changes (see Cargo.toml).
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
PROFILE		?=	release
ELF		:=	target/armv6k-nintendo-3ds/$(PROFILE)/cement-server.elf

# tools (override on the command line if they live elsewhere)
CARGO		?=	cargo +nightly
MAKEROM		?=	makerom
BANNERTOOL	?=	bannertool

export PATH := $(DEVKITARM)/bin:$(DEVKITPRO)/tools/bin:$(PATH)

# ctru-sys reads the gcc version from the wrong word of some devkitARM builds'
# "gcc --version", so bindgen misses the compiler headers: give it the folder
GCC_VERSION := $(shell $(DEVKITARM)/bin/arm-none-eabi-gcc -dumpversion)
export BINDGEN_EXTRA_CLANG_ARGS ?= -isystem $(DEVKITARM)/lib/gcc/arm-none-eabi/$(GCC_VERSION)/include

# version shown on the 3DS and reported to clients: the VERSION file as it is
# (major.minor.micro, bumped by hand for a release), plus -<short commit>-dev
# when there are uncommitted changes or for a fast build
GIT          := $(shell command -v git 2>/dev/null || (test -x "/c/Program Files/Git/cmd/git.exe" && echo "/c/Program Files/Git/cmd/git.exe"))
BASE_VERSION := $(strip $(shell tr -d '\r' < VERSION 2>/dev/null))
ifeq ($(strip $(APP_VERSION)),)
COMMITS      := $(strip $(shell "$(GIT)" rev-list --count HEAD 2>/dev/null))
COMMIT       := $(strip $(shell "$(GIT)" rev-parse --short HEAD 2>/dev/null))
# line endings and file modes differ between a Windows checkout and WSL: not changes
DIRTY        := $(if $(strip $(shell "$(GIT)" -c core.autocrlf=true -c core.filemode=false status --porcelain 2>/dev/null)),-dev,)
ifneq ($(PROFILE),release)
DIRTY        := -dev
endif
APP_VERSION  := $(if $(COMMITS),$(BASE_VERSION)$(if $(DIRTY),-$(COMMIT)-dev,),$(BASE_VERSION)-dev)
endif

# CIA title version = the VERSION file's major.minor.micro, so FBI and the
# system settings show the same version as Cement. Builds between releases
# share it: the 3DS refuses to install one over the same or a newer version
# (D8E08027) unless the installed copy is deleted first (FBI does that, Cement
# can't for itself). makerom clears the minor bits of -ver, so
# tools/set-cia-version.sh writes the version into the TMD after it.

# $(call package,<file name>,<title>,<banner>,<icon>,<elf>): the .cia and the .3dsx
define package
	@mkdir -p $(BUILD)/$(1)
	@$(BANNERTOOL) makebanner -i $(3) -a meta/audio.wav -o $(BUILD)/$(1)/banner.bnr
	@$(BANNERTOOL) makesmdh -s "$(2)" -l "Fast FTP + CIA installer v$(APP_VERSION)" -p "Synaelle" -i $(4) -o $(BUILD)/$(1)/icon.icn
	@$(MAKEROM) -f cia -o $(1).cia -target t -exefslogo -rsf meta/$(1).rsf 		-elf $(5) -icon $(BUILD)/$(1)/icon.icn -banner $(BUILD)/$(1)/banner.bnr
	@sh tools/set-cia-version.sh $(1).cia $(BASE_VERSION)
	@cp $(5) $(1).elf
	@echo built ... $(1).cia v$(APP_VERSION)
	@3dsxtool $(1).elf $(1).3dsx --smdh=$(BUILD)/$(1)/icon.icn
	@echo built ... $(1).3dsx v$(APP_VERSION)
endef

.PHONY: all cement clean

all: cement

cement:
	@CEMENT_VERSION="$(APP_VERSION)" $(CARGO) 3ds build --profile $(PROFILE)
	$(call package,Cement,Cement,meta/banner.png,meta/icon.png,$(ELF))

clean:
	@echo clean ...
	@rm -fr $(BUILD) target Cement.elf Cement.cia Cement.3dsx

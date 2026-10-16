#---------------------------------------------------------------------------------
# Cement 3DS server. "make" builds Cement.cia (and its .elf).
# Based on devkitPro's 3ds application template.
#---------------------------------------------------------------------------------
.SUFFIXES:

ifeq ($(strip $(DEVKITARM)),)
$(error "Please set DEVKITARM in your environment. export DEVKITARM=<path to>devkitARM")
endif

TOPDIR ?= $(CURDIR)
include $(DEVKITARM)/3ds_rules

TARGET		:=	Cement
BUILD		:=	build
SOURCES		:=	source
INCLUDES	:=	include

# CIA tools (override on the command line if they live elsewhere)
MAKEROM		?=	makerom
BANNERTOOL	?=	bannertool

# version shown on the 3DS and reported to clients:
# <../VERSION>.<commit count>-<short commit>, plus -dev when there are uncommitted changes
GIT          := $(shell command -v git 2>/dev/null || (test -x "/c/Program Files/Git/cmd/git.exe" && echo "/c/Program Files/Git/cmd/git.exe"))
ifeq ($(strip $(APP_VERSION)),)
BASE_VERSION := $(strip $(shell cat ../VERSION 2>/dev/null))
COMMITS      := $(strip $(shell "$(GIT)" rev-list --count HEAD 2>/dev/null))
COMMIT       := $(strip $(shell "$(GIT)" rev-parse --short HEAD 2>/dev/null))
DIRTY        := $(if $(strip $(shell "$(GIT)" status --porcelain 2>/dev/null)),-dev,)
APP_VERSION  := $(if $(COMMITS),$(BASE_VERSION).$(COMMITS)-$(COMMIT)$(DIRTY),$(BASE_VERSION)-dev)
export APP_VERSION
endif

# CIA title version = build number. The HOME menu caches icons per title ID and
# title version, so it has to go up or updates keep showing the old icon.
TITLE_VERSION := $(or $(strip $(shell "$(GIT)" rev-list --count HEAD 2>/dev/null)),0)

ARCH	:=	-march=armv6k -mtune=mpcore -mfloat-abi=hard -mtp=soft

CFLAGS	:=	-Wall -O2 -mword-relocations -ffunction-sections $(ARCH)
CFLAGS	+=	$(INCLUDE) -D__3DS__ -DAPP_VERSION=\"$(APP_VERSION)\"
ASFLAGS	:=	$(ARCH)
LDFLAGS	=	-specs=3dsx.specs $(ARCH) -Wl,-Map,$(notdir $*.map)

LIBS	:= -lctru -lm
LIBDIRS	:= $(CTRULIB)

ifneq ($(BUILD),$(notdir $(CURDIR)))

export OUTPUT	:=	$(CURDIR)/$(TARGET)
export TOPDIR	:=	$(CURDIR)
export VPATH	:=	$(foreach dir,$(SOURCES),$(CURDIR)/$(dir))
export DEPSDIR	:=	$(CURDIR)/$(BUILD)

CFILES		:=	$(foreach dir,$(SOURCES),$(notdir $(wildcard $(dir)/*.c)))
export LD	:=	$(CC)
export OFILES	:=	$(CFILES:.c=.o)
export INCLUDE	:=	$(foreach dir,$(INCLUDES),-I$(CURDIR)/$(dir)) \
			$(foreach dir,$(LIBDIRS),-I$(dir)/include) \
			-I$(CURDIR)/$(BUILD)
export LIBPATHS	:=	$(foreach dir,$(LIBDIRS),-L$(dir)/lib)

.PHONY: all clean

all: $(BUILD)
	@$(MAKE) --no-print-directory -C $(BUILD) -f $(CURDIR)/Makefile
	@$(BANNERTOOL) makebanner -i meta/banner.png -a meta/audio.wav -o $(BUILD)/banner.bnr
	@$(BANNERTOOL) makesmdh -s "Cement" -l "Fast FTP + CIA installer v$(APP_VERSION)" -p "Synaelle" -i meta/icon.png -o $(BUILD)/icon.icn
	@$(MAKEROM) -f cia -o $(TARGET).cia -target t -exefslogo -rsf meta/$(TARGET).rsf -ver $(TITLE_VERSION) \
		-elf $(TARGET).elf -icon $(BUILD)/icon.icn -banner $(BUILD)/banner.bnr
	@echo built ... $(TARGET).cia v$(APP_VERSION)

$(BUILD):
	@mkdir -p $@

clean:
	@echo clean ...
	@rm -fr $(BUILD) $(TARGET).elf $(TARGET).cia

else

$(OUTPUT).elf	:	$(OFILES)

-include $(DEPSDIR)/*.d

endif

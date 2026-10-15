#---------------------------------------------------------------------------------
# synaftp 3DS server. "make" builds synaftp.cia (and the .elf it is made from).
# Based on devkitPro's 3ds application template.
#---------------------------------------------------------------------------------
.SUFFIXES:

ifeq ($(strip $(DEVKITARM)),)
$(error "Please set DEVKITARM in your environment. export DEVKITARM=<path to>devkitARM")
endif

TOPDIR ?= $(CURDIR)
include $(DEVKITARM)/3ds_rules

TARGET		:=	synaftp
BUILD		:=	build
SOURCES		:=	source
INCLUDES	:=	include

# CIA tools (override on the command line if they live elsewhere)
MAKEROM		?=	makerom
BANNERTOOL	?=	bannertool

ARCH	:=	-march=armv6k -mtune=mpcore -mfloat-abi=hard -mtp=soft

CFLAGS	:=	-Wall -O2 -mword-relocations -ffunction-sections $(ARCH)
CFLAGS	+=	$(INCLUDE) -D__3DS__
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
	@$(BANNERTOOL) makesmdh -s "synaftp" -l "Fast FTP server (SD card)" -p "Synaelle" -i meta/icon.png -o $(BUILD)/icon.icn
	@$(MAKEROM) -f cia -o $(TARGET).cia -target t -exefslogo -rsf meta/$(TARGET).rsf \
		-elf $(TARGET).elf -icon $(BUILD)/icon.icn -banner $(BUILD)/banner.bnr
	@echo built ... $(TARGET).cia

$(BUILD):
	@mkdir -p $@

clean:
	@echo clean ...
	@rm -fr $(BUILD) $(TARGET).elf $(TARGET).cia

else

$(OUTPUT).elf	:	$(OFILES)

-include $(DEPSDIR)/*.d

endif

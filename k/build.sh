#!/bin/sh

vasm6502_oldstyle -sect -dotdir -ce02 -Fbin -o k.bin k.asm \
	&& hexdump -C k.bin

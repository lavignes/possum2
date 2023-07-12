#!/bin/sh

vasm6502_oldstyle -dotdir -ce02 -Fbin -o k.bin k.asm
hexdump -C k.bin

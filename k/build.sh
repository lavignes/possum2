#!/bin/sh

pasm -o k.bin -s k.sym k.asm \
	&& hexdump -C k.bin

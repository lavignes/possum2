; vim: ft=pasm sw=8 ts=8 cc=80 noet
SER0_DATA	equ $F010
SER0_STATUS	equ $F011
SER0_CMD	equ $F012
SER0_CTRL	equ $F013

BANK0		equ $F000
INT_LATCH	equ $F0FF

		bss
*		equ $0200

		txt
*		equ $F100

Reset		sta SER0_STATUS		; reset uart
		lda #$09		; rx interrupt enable, turn on
		sta SER0_CMD
		cli
		bru *

Ser0Tx		pha
.wait		lda SER0_STATUS
		and #$10		; wait for empty buf
		beq .wait
		pla
		sta SER0_DATA
		rts

Ser0Rx		lda SER0_STATUS
		and #$08		; wait for buf full
		beq Ser0Rx
		lda SER0_DATA
		rts

Irq		pha			; store a and x on caller stack
		phx

		lda BANK0
		tax
		lda #0
		sta BANK0		; switched to ch 0
		phx			; store previous bank on k stack

		; todo: need to check if BRK flag is set
		;   and go to a special BRK handler

		ldx INT_LATCH		; value is multiple of 2
		jmp (.table,x)
.table		wrd 0			; value is always at least 2
		wrd Fdc0Drq
		wrd Fdc1Drq
		wrd Fdc0Irq
		wrd Fdc1Irq
		wrd Ser0Irq
		wrd Ser1Irq

.restore	pla
		sta BANK0		; restore bank 0
		plx			; restore a and x from caller stack
		pla
		rti

Fdc0Drq		bru Irq.restore

Fdc1Drq		bru Irq.restore

Fdc0Irq		bru Irq.restore

Fdc1Irq		bru Irq.restore

Ser0Irq		bsr Ser0Rx
		bsr Ser0Tx
		bru Irq.restore

Ser1Irq		bru Irq.restore

Nmi		rti

		pad $FFFA-*
		wrd Nmi,Reset,Irq


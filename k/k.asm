; vim: ft=vasm65CE02
SER0_DATA	.equ $F010
SER0_STATUS	.equ $F011
SER0_CMD	.equ $F012
SER0_CTRL	.equ $F013

BANK0		.equ $F000

INT_LATCH	.equ $F0FF

SECTOR_PTR	.ezp $00

*	.equ $F100
vReset:
	sta SER0_STATUS	; reset uart
	lda #$0B	; disable uart interrupts, enable tx/rx
	sta SER0_CMD

.loop:
	bsr ser0Rx
	bsr ser0Tx
	bru .loop

ser0Tx:
	pha
.txWait:
	lda SER0_STATUS
	and #$10	; wait for tx buffer empty
	beq .txWait
	pla
	sta SER0_DATA
	rts

ser0Rx:
	lda SER0_STATUS
	and #$08	; wait for rx buffer full
	beq ser0Rx
	lda SER0_DATA
	rts

vNmi:
	rti

vIrq:
	pha		; store a and x on caller stack
	phx

	lda BANK0
	tax
	lda 0
	sta BANK0	; switched to ch 0
	phx		; store previous chapter

	ldx INT_LATCH	; value is multiple of 2
	jmp (.table,x)
.table:
	.dw fdc0Drq
	.dw fdc1Drq
	.dw fdc0Irq
	.dw fdc1Irq
	.dw ser0Irq
	.dw ser1Irq

.restore:
	pla
	sta BANK0	; restore bank 0

	plx		; restore a and x from caller stack
	pla
	rti

fdc0Drq:
	inw SECTOR_PTR
	bru vIrq.restore

fdc1Drq:
	bru vIrq.restore

fdc0Irq:
	bru vIrq.restore

fdc1Irq:
	bru vIrq.restore

ser0Irq:
	bru vIrq.restore

ser1Irq:
	bru vIrq.restore

; Vector Table
*	.equ $FFFA
	.dw vNmi
	.dw vReset
	.dw vIrq

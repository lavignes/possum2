; vim: ft=vasm65CE02
q	.ezp $42

SER0_DATA	.equ $F010
SER0_STATUS	.equ $F011
SER0_CMD	.equ $F012
SER0_CTRL	.equ $F013

*	.equ $F100
kMain:
	sta SER0_STATUS	; reset uart
	lda #$0B	; disable interrupts, enable tx/rx
	sta SER0_CMD

.loop:
	bsr ser0Rx
	bsr ser0Tx
	bra .loop

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

kNMI:
	rti

kIRQ:
	rti

; Vector Table
*	.equ $FFFA
	.dw kNMI
	.dw kMain
	.dw kIRQ

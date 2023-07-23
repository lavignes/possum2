; vim: ft=pasm sw=8 ts=8 cc=80 noet
SER0_DATA		equ $F010
SER0_STATUS		equ $F011
SER0_CMD		equ $F012
SER0_CTRL		equ $F013

BANK0			equ $F000
INT_LATCH		equ $F0FF

*			equ $F100


vReset			sta SER0_STATUS		; reset uart
			lda #$0B		; disable interrupts
			sta SER0_CMD		; enable tx/rx
.loop			bsr ser0Rx
			bsr ser0Tx
			bru .loop

ser0Tx			pha
.txWait			lda SER0_STATUS
			and #$10		; wait for empty buf
			beq .txWait
			pla
			sta SER0_DATA
			rts

ser0Rx			lda SER0_STATUS
			and #$08		; wait for buf full
			beq ser0Rx
			lda SER0_DATA
			rts

vNmi			rti

vIrq			rti

			pad $FFFA-*
			wrd vNmi
			wrd vReset
			wrd vIrq

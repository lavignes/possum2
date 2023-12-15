; vim: ft=pasm sw=8 ts=8 cc=80 noet

PUSH_ALL	mac
		pha
		tba
		pha
		phx
		phy
		phz
		end

PULL_ALL	mac
		plz
		ply
		plx
		pla
		tab
		pla
		end

SER0_DATA	equ $F010
SER0_STATUS	equ $F011
SER0_CMD 	equ $F012
SER0_CTRL	equ $F013

BANK0		equ $F000
INT_LATCH	equ $F0FF

		bss
*		equ $0000
ksp		pad 2

		txt
*		equ $F100

; copy A to (SP) Y times
MemSet		cpy #0
		jmp .chk	; todo: a bru .chk computes the wrong addr!
.set		dey
		sta (2,sp),y
.chk		bne .set

		plx
		ply
		pla		; remove arg from stack
		pla		; could be a macro candidate
		phy
		phx

		rts

Reset		cle     	; enable extended SP
		ldy #$F0	; and set SP to $F000
		tys

		lda #>BANK0
		pha
		lda #<BANK0
		pha
		lda #0
		ldy #15
		bsr MemSet

		sta SER0_STATUS	; reset uart
		lda #$09	; rx interrupt enable, turn on
		sta SER0_CMD
		cli
		bru *

Ser0Tx		pha
.wait		lda SER0_STATUS
		and #$10	; wait for empty buf
		beq .wait
		pla
		sta SER0_DATA
		rts

Ser0Rx		lda SER0_STATUS
		and #$08	; wait for buf full
		beq Ser0Rx
		lda SER0_DATA
		rts

Irq
		PUSH_ALL	; store regs on user stack

		ldy #5		; read user flags off user stack
		lda (0,sp),y	; and save them in B for a while
		tab

		tsx		; load user SP in XY
		tsy

		ldz BANK0	; save user bank 0 in Z
		lda #0
		sta BANK0	; switch to kernel bank 0

		txa		; restore kernel SP
		ldx |ksp+0
		txs
		tax
		tya
		ldy |ksp+1
		tys
		tay

		phx		; store user SP on kernel stack
		phy
		phz		; store user bank 0 on kernel stack

		;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;
		; finally ready to handle interrupts!
		;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;

		tba		; check for BRK user flag
		and #%00010000
		bne BrkIrq

		ldx INT_LATCH	; value is multiple of 2
		jmp (.table,x)
.table		pad 2		; value is always at least 2
		wrd Fdc0Drq	; so we need a blank spot
		wrd Fdc1Drq
		wrd Fdc0Irq
		wrd Fdc1Irq
		wrd Ser0Irq
		wrd Ser1Irq

IrqRet		pla		; load user SP into XY and
		ply		; restore user bank 0
		plx
		sta BANK0

		txs		; restore user SP
		tys

		PULL_ALL	; restore regs from user stack
		rti

Fdc0Drq		bru IrqRet
Fdc1Drq		bru IrqRet
Fdc0Irq		bru IrqRet
Fdc1Irq		bru IrqRet
Ser0Irq		bsr Ser0Rx
		bsr Ser0Tx
		bru IrqRet
Ser1Irq		bru IrqRet
BrkIrq		bru IrqRet

Nmi		rti

		pad $FFFA-*
		wrd Nmi,Reset,Irq


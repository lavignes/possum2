; vim: ft=vasm65CE02
q	.ezp $42

*	.equ $F100
kMain:
	jmp kMain

kNMI:
	rti

kIRQ:
	rti

; Vector Table
*	.equ $FFFA
	.dw kNMI
	.dw kMain
	.dw kIRQ

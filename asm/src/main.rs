use std::{
    error::Error,
    fs::File,
    io::{self, ErrorKind, Read, Seek, Write},
    path::PathBuf,
    process::ExitCode,
    str,
};

use clap::Parser;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    /// Input file
    input: PathBuf,

    /// Output file (default: stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Symbol file
    #[arg(short, long)]
    sym: Option<PathBuf>,
}

fn main() -> ExitCode {
    if let Err(e) = main_real() {
        eprintln!("{e}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn main_real() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let file = File::open(args.input).map_err(|e| format!("cannot open file: {e}"))?;
    let reader = Reader::new(file);
    let lexer = Lexer::new(reader);
    let output: Box<dyn Write> = match args.output {
        Some(path) => Box::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .map_err(|e| format!("cannot open file: {e}"))?,
        ),
        None => Box::new(io::stdout()),
    };

    let mut asm = Asm::new(lexer, output);
    eprint!("pass1: ");
    pass(&mut asm)?;
    eprintln!("ok");

    asm.rewind()?;

    eprint!("pass2: ");
    pass(&mut asm)?;
    eprintln!("ok");

    if let Some(path) = args.sym {
        let mut file = File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| format!("cannot open file: {e}"))?;
        for sym in asm.syms {
            writeln!(&mut file, "{}:{:04X}", sym.0, sym.1)?;
        }
    }

    Ok(())
}

fn pass(asm: &mut Asm) -> Result<(), Box<dyn Error>> {
    loop {
        if asm.lexer_mut().peek()? == EOF {
            if asm.lexers.len() > 1 {
                asm.lexers.pop();
            } else {
                break;
            }
        }

        // special case: setting PC
        if asm.lexer_mut().peek()? == STAR {
            asm.lexer_mut().eat();
            if asm.lexer_mut().peek()? != IDENT && !asm.lexer().string().eq_ignore_ascii_case("EQU")
            {
                Err(asm.lexer().err("expected EQU"))?;
            }
            asm.lexer_mut().eat();
            let expr = expr(asm)?;
            let expr = const_expr(asm, expr)?;
            let pc = const_word(asm, expr)?;
            asm.set_pc(pc);
            end_of_line(asm)?;
            continue;
        }

        // macro?
        if asm.lexer_mut().peek()? == IDENT {
            if let Some(mac) = asm
                .macros
                .iter()
                .find(|mac| mac.name == asm.lexer().string())
                .cloned()
            {
                asm.lexer_mut().eat();
                let mut args = Vec::new();
                let mut arg_strings = Vec::new();
                loop {
                    match asm.lexer_mut().peek()? {
                        NEWLINE | EOF => {
                            break;
                        }
                        tok @ (IDENT | STRING) => {
                            args.push(MacroToken {
                                inner: tok,
                                string_index: arg_strings.len(),
                                number: 0,
                                line: asm.lexer().line(),
                            });
                            arg_strings.push(asm.lexer().string().to_string());
                        }
                        NUMBER => args.push(MacroToken {
                            inner: NUMBER,
                            string_index: 0,
                            number: asm.lexer().number(),
                            line: asm.lexer().line(),
                        }),
                        tok => args.push(MacroToken {
                            inner: tok,
                            string_index: 0,
                            number: 0,
                            line: asm.lexer().line(),
                        }),
                    }
                    asm.lexer_mut().eat();
                    if asm.lexer_mut().peek()? != COMMA {
                        break;
                    }

                    asm.lexer_mut().eat();
                }
                end_of_line(asm)?;
                // todo: invocation constructor
                let invocation = MacroInvocation {
                    inner: mac,
                    invocation_line: asm.lexer().line(),
                    pos: 0,
                    string: String::new(),
                    args,
                    arg_strings,
                };
                asm.lexers.push(Box::new(invocation));
                continue;
            }
        }

        // label?
        if asm.lexer_mut().peek()? == IDENT
            && !asm.lexer().string().eq_ignore_ascii_case("EQU")
            && !asm.lexer().string().eq_ignore_ascii_case("MAC")
            && !OPS
                .iter()
                .any(|op| asm.lexer().string().eq_ignore_ascii_case(op.0))
            && !POPS
                .iter()
                .any(|op| asm.lexer().string().eq_ignore_ascii_case(op.0))
        {
            // apply outer label
            if asm.lexer().string().starts_with(".") {
                // todo: remove clone
                let outer_label = asm.outer_label.clone();
                asm.lexer_mut().prepend_string(&outer_label);
            } else {
                asm.outer_label = asm.lexer().string().to_string();
            }

            let name = asm.lexer().string().to_string();
            asm.lexer_mut().eat();

            // check if this label is being defined to a macro
            if asm.lexer_mut().peek()? == IDENT && asm.lexer().string().eq_ignore_ascii_case("MAC")
            {
                asm.lexer_mut().eat();
                if asm.macros.iter().any(|mac| mac.name == name) {
                    // todo: it shouldnt even be possible for this to happen
                    // if we try to define the macro again, it would immediately invoke it
                    return Err(asm.lexer().err("macro already defined"))?;
                }
                mac(asm, name)?;
                continue;
            }

            // is this already in the symbol table?
            let sym_index =
                if let Some(item) = asm.syms.iter().enumerate().find(|item| item.1 .0 == name) {
                    // allowed to redef during second pass
                    // todo: should test if label value didnt change
                    if !asm.emit {
                        Err(asm.lexer().err("symbol already defined"))?
                    }
                    item.0
                } else {
                    // save the label in the symbol table
                    let index = asm.syms.len();
                    asm.syms.push((name, 0));
                    index
                };

            // check if this label is being defined to a value
            if asm.lexer_mut().peek()? == IDENT && asm.lexer().string().eq_ignore_ascii_case("EQU")
            {
                asm.lexer_mut().eat();
                let expr = expr(asm)?;
                if asm.emit {
                    asm.syms[sym_index].1 = const_expr(asm, expr)?;
                } else if let Some(expr) = expr {
                    asm.syms[sym_index].1 = expr;
                } else {
                    // we couldn't evaluate this yet, so remove it
                    asm.syms.pop();
                }
                end_of_line(asm)?;
                continue;
            }

            // otherwise it is a pointer to the current PC
            asm.syms[sym_index].1 = asm.pc() as u32 as i32;
        }

        if asm.bss_mode {
            // only pad, adj, txt, and inf work in bss
            if asm.lexer_mut().peek()? == IDENT && asm.lexer().string().eq_ignore_ascii_case("PAD")
            {
                asm.lexer_mut().eat();
                pad(asm)?;
                continue;
            }
            if asm.lexer_mut().peek()? == IDENT && asm.lexer().string().eq_ignore_ascii_case("ADJ")
            {
                asm.lexer_mut().eat();
                adj(asm)?;
                continue;
            }
            if asm.lexer_mut().peek()? == IDENT && asm.lexer().string().eq_ignore_ascii_case("TXT")
            {
                asm.lexer_mut().eat();
                txt(asm)?;
                continue;
            }
            if asm.lexer_mut().peek()? == IDENT && asm.lexer().string().eq_ignore_ascii_case("INF")
            {
                asm.lexer_mut().eat();
                inf(asm)?;
                continue;
            }
        } else {
            // pseudo op?
            if asm.lexer_mut().peek()? == IDENT {
                if let Some(pop) = POPS
                    .iter()
                    .find(|pop| asm.lexer().string().eq_ignore_ascii_case(pop.0))
                {
                    asm.lexer_mut().eat();
                    // evaluate the pseudo op
                    pop.1(asm)?;
                    continue;
                }
            }

            // op?
            if asm.lexer_mut().peek()? == IDENT {
                let op = OPS
                    .iter()
                    .find(|op| asm.lexer().string().eq_ignore_ascii_case(op.0))
                    .ok_or_else(|| asm.lexer().err("unknown opcode"))?;
                asm.lexer_mut().eat();
                operand(asm, op)?;
            }
        }

        end_of_line(asm)?;
    }
    Ok(())
}

const IMM: u8 = 0;
const ABS: u8 = 1;
const BP: u8 = 2;
const ACCUM: u8 = 3;
const IMPL: u8 = 4;
const IND_X: u8 = 5; // (BP,X)
const IND_Y: u8 = 6; // (BP),Y
const IND_Z: u8 = 7; // (BP),Z
const IND_SP: u8 = 8; // (d,SP),Y
const BP_X: u8 = 9;
const BP_Y: u8 = 10;
const ABS_X: u8 = 11;
const ABS_Y: u8 = 12;
const REL: u8 = 13;
const WREL: u8 = 14;
const IND_ABS: u8 = 15; // (ABS)
const BP_REL: u8 = 16;
const IND_ABS_X: u8 = 17; // (ABS,X)

type Op = (&'static str, &'static [(u8, u8)]);

#[rustfmt::skip]
const OPS: &[Op] = &[
    ("AUG", &[(IMPL, 0x5C)]), // special
    ("BRK", &[(IMPL, 0x00)]), // special
    ("CLC", &[(IMPL, 0x18)]),
    ("CLD", &[(IMPL, 0xD8)]),
    ("CLE", &[(IMPL, 0x02)]),
    ("CLI", &[(IMPL, 0x58)]),
    ("CLV", &[(IMPL, 0xB8)]),
    ("DEX", &[(IMPL, 0xCA)]),
    ("DEY", &[(IMPL, 0x88)]),
    ("DEZ", &[(IMPL, 0x3B)]),
    ("INX", &[(IMPL, 0xE8)]),
    ("INY", &[(IMPL, 0xC8)]),
    ("INZ", &[(IMPL, 0x1B)]),
    ("NOP", &[(IMPL, 0xEA)]),
    ("PHA", &[(IMPL, 0x48)]),
    ("PHP", &[(IMPL, 0x08)]),
    ("PHX", &[(IMPL, 0xDA)]),
    ("PHY", &[(IMPL, 0x5A)]),
    ("PHZ", &[(IMPL, 0xDB)]),
    ("PLA", &[(IMPL, 0x68)]),
    ("PLP", &[(IMPL, 0x28)]),
    ("PLX", &[(IMPL, 0xFA)]),
    ("PLY", &[(IMPL, 0x7A)]),
    ("PLZ", &[(IMPL, 0xFB)]),
    ("RTI", &[(IMPL, 0x40)]),
    ("RTN", &[(IMPL, 0x62)]), // special
    ("RTS", &[(IMPL, 0x60)]),
    ("SEC", &[(IMPL, 0x38)]),
    ("SED", &[(IMPL, 0xF8)]),
    ("SEE", &[(IMPL, 0x03)]),
    ("SEI", &[(IMPL, 0x78)]),
    ("TAB", &[(IMPL, 0x5B)]),
    ("TAX", &[(IMPL, 0xAA)]),
    ("TAY", &[(IMPL, 0xA8)]),
    ("TBA", &[(IMPL, 0x7B)]),
    ("TSX", &[(IMPL, 0xBA)]),
    ("TSY", &[(IMPL, 0x0B)]),
    ("TXA", &[(IMPL, 0x8A)]),
    ("TXS", &[(IMPL, 0x9A)]),
    ("TYA", &[(IMPL, 0x98)]),
    ("TYS", &[(IMPL, 0x2B)]),
    ("TZA", &[(IMPL, 0x6B)]),

    ("ADC", &[(IMM, 0x69), (ABS, 0x6D), (BP, 0x65), (IND_X, 0x61), (IND_Y, 0x71), (IND_Z, 0x72), (BP_X, 0x75), (ABS_X, 0x7D), (ABS_Y, 0x79)]),
    ("AND", &[(IMM, 0x29), (ABS, 0x2D), (BP, 0x25), (IND_X, 0x21), (IND_Y, 0x31), (IND_Z, 0x32), (BP_X, 0x35), (ABS_X, 0x3D), (ABS_Y, 0x39)]),
    ("ASL", &[(ABS, 0x0E), (BP, 0x06), (ACCUM, 0x0A), (BP_X, 0x16), (ABS_X, 0x1E)]),
    ("ASR", &[(BP, 0x44), (ACCUM, 0x43), (BP_X, 0x54)]),
    ("ASW", &[(ABS, 0xCB)]),
    ("BIT", &[(IMM, 0x89), (ABS, 0x2C), (BP, 0x24), (BP_X, 0x34), (ABS_X, 0x3C)]),
    ("BBR", &[(BP_REL, 0x0F), (BP_REL, 0x1F), (BP_REL, 0x2F), (BP_REL, 0x3F), (BP_REL, 0x4F), (BP_REL, 0x5F), (BP_REL, 0x6F), (BP_REL, 0x7F)]), // special
    ("BBS", &[(BP_REL, 0x8F), (BP_REL, 0x9F), (BP_REL, 0xAF), (BP_REL, 0xBF), (BP_REL, 0xCF), (BP_REL, 0xDF), (BP_REL, 0xEF), (BP_REL, 0xFF)]), // special
    ("BCC", &[(REL, 0x90), (WREL, 0x93)]),
    ("BCS", &[(REL, 0xB0), (WREL, 0xB3)]),
    ("BEQ", &[(REL, 0xF0), (WREL, 0xF3)]),
    ("BMI", &[(REL, 0x30), (WREL, 0x33)]),
    ("BNE", &[(REL, 0xD0), (WREL, 0xD3)]),
    ("BPL", &[(REL, 0x10), (WREL, 0x13)]),
    ("BRU", &[(REL, 0x80), (WREL, 0x83)]),
    ("BSR", &[(WREL, 0x63)]),
    ("BVC", &[(REL, 0x50), (WREL, 0x53)]),
    ("BVS", &[(REL, 0x70), (WREL, 0x73)]),
    ("CMP", &[(IMM, 0xC9), (ABS, 0xCD), (BP, 0xC5), (IND_X, 0xC1), (IND_Y, 0xD1), (IND_Z, 0xD2), (BP_X, 0xD5), (ABS_X, 0xDD), (ABS_Y, 0xD9)]),
    ("DEC", &[(ABS, 0xCE), (BP, 0xC6), (ACCUM, 0x3A), (BP_X, 0xD6), (ABS_X, 0xDE)]),
    ("EOR", &[(IMM, 0x49), (ABS, 0x4D), (BP, 0x45), (IND_X, 0x41), (IND_Y, 0x51), (IND_Z, 0x52), (BP_X, 0x55), (ABS_X, 0x5D), (ABS_Y, 0x59)]),
    ("INC", &[(ABS, 0xEE), (BP, 0xE6), (ACCUM, 0x1A), (BP_X, 0xF6), (ABS_X, 0xFE)]),
    ("INW", &[(BP, 0xE3)]),
    ("JMP", &[(ABS, 0x4C), (IND_ABS, 0x6C), (IND_ABS_X, 0x7C)]),
    ("JSR", &[(ABS, 0x20), (IND_ABS, 0x22), (IND_ABS_X, 0x23)]),
    ("LDA", &[(IMM, 0xA9), (ABS, 0xAD), (BP, 0xA5), (IND_X, 0xA1), (IND_Y, 0xB1), (IND_Z, 0xB2), (IND_SP, 0xE2), (BP_X, 0xB5), (ABS_X, 0xBD), (ABS_Y, 0xB9)]),
    ("LDX", &[(IMM, 0xA2), (ABS, 0xAE), (BP, 0xA6), (BP_Y, 0xB6), (ABS_Y, 0xBE)]),
    ("LDY", &[(IMM, 0xA0), (ABS, 0xAC), (BP, 0xA4), (BP_X, 0xB4), (ABS_X, 0xBC)]),
    ("LDZ", &[(IMM, 0xA3), (ABS, 0xAB), (ABS_X, 0xBB)]),
    ("LSR", &[(ABS, 0x4E), (BP, 0x46), (ACCUM, 0x4A), (BP_X, 0x56), (ABS_X, 0x5E)]),
    ("NEG", &[(ACCUM, 0x42)]),
    ("ORA", &[(IMM, 0x09), (ABS, 0x0D), (BP, 0x05), (IND_X, 0x01), (IND_Y, 0x11), (IND_Z, 0x12), (BP_X, 0x15), (ABS_X, 0x1D), (ABS_Y, 0x19)]),
    ("RMB", &[(BP, 0x07), (BP, 0x17), (BP, 0x27), (BP, 0x37), (BP, 0x47), (BP, 0x57), (BP, 0x67), (BP, 0x77)]), // special
    ("ROL", &[(ABS, 0x2E), (BP, 0x26), (ACCUM, 0x2A), (BP_X, 0x36), (ABS_X, 0x3E)]),
    ("ROR", &[(ABS, 0x6E), (BP, 0x66), (ACCUM, 0x6A), (BP_X, 0x76), (ABS_X, 0x7E)]),
    ("ROW", &[(ABS, 0xEB)]),
    ("SBC", &[(IMM, 0xE9), (ABS, 0xED), (BP, 0xE5), (IND_X, 0xE1), (IND_Y, 0xF1), (IND_Z, 0xF2), (BP_X, 0xF5), (ABS_X, 0xFD), (ABS_Y, 0xF9)]),
    ("SMB", &[(BP, 0x87), (BP, 0x97), (BP, 0xA7), (BP, 0xB7), (BP, 0xC7), (BP, 0xD7), (BP, 0xE7), (BP, 0xF7)]), // special
    ("STA", &[(ABS, 0x8D), (BP, 0x85), (IND_X, 0x81), (IND_Y, 0x91), (IND_Z, 0x92), (IND_SP, 0x82), (BP_X, 0x95), (ABS_X, 0x9D), (ABS_Y, 0x99)]),
    ("STX", &[(ABS, 0x8E), (BP, 0x86), (ABS_Y, 0x96), (ABS_Y, 0x9B)]),
    ("STY", &[(ABS, 0x8C), (BP, 0x84), (ABS_X, 0x94), (ABS_X, 0x8B)]),
    ("STZ", &[(ABS, 0x9C), (BP, 0x64), (ABS_X, 0x74), (ABS_X, 0x9E)]),
    ("TRB", &[(ABS, 0x1C), (BP, 0x14)]), // xfer reset bits, M[addr] &= ~A
    ("TSB", &[(ABS, 0x0C), (BP, 0x04)]), // xfer set bits, M[addr] |= A
];

fn operand(asm: &mut Asm, op: &Op) -> io::Result<()> {
    // implied?
    if (op.1.len() == 1) && (op.1[0].0 == IMPL) {
        let opcode = op.1[0].1;
        if asm.emit {
            asm.write(&[opcode])?;
        }
        asm.add_pc(1)?;
        // handle the few special cases longer than 1 byte
        if op.0.eq_ignore_ascii_case("AUG") {
            if asm.emit {
                asm.write(&[0xEA, 0xEA, 0xEA])?;
            }
            asm.add_pc(3)?;
        } else if op.0.eq_ignore_ascii_case("BRK") {
            if asm.emit {
                asm.write(&[0xEA])?;
            }
            asm.add_pc(1)?;
        } else if op.0.eq_ignore_ascii_case("RTN") {
            let expr = expr(asm)?;
            if asm.emit {
                let expr = const_expr(asm, expr)?;
                let byte = const_byte(asm, expr)?;
                asm.write(&[byte])?;
            }
            asm.add_pc(1)?;
        }
        return Ok(());
    }

    // immediate?
    if asm.lexer_mut().peek()? == HASH {
        asm.lexer_mut().eat();
        if let Some((_, opcode)) = op.1.iter().find(|(mode, _)| *mode == IMM) {
            if asm.emit {
                asm.write(&[*opcode])?;
            }
            asm.add_pc(1)?;
            let expr = expr(asm)?;
            if op.0.eq_ignore_ascii_case("PHW") {
                if asm.emit {
                    let expr = const_expr(asm, expr)?;
                    let word = const_word(asm, expr)?.to_le_bytes();
                    asm.write(&word)?;
                }
                asm.add_pc(2)?;
            } else {
                if asm.emit {
                    let expr = const_expr(asm, expr)?;
                    let byte = const_byte(asm, expr)?;
                    asm.write(&[byte])?;
                }
                asm.add_pc(1)?;
            }
            return Ok(());
        }
        return Err(asm.lexer().err("illegal addressing mode"));
    }

    // accum?
    if asm.lexer_mut().peek()? == UPPERA {
        asm.lexer_mut().eat();
        if let Some((_, opcode)) = op.1.iter().find(|(mode, _)| *mode == ACCUM) {
            if asm.emit {
                asm.write(&[*opcode])?;
            }
            asm.add_pc(1)?;
            return Ok(());
        }
        return Err(asm.lexer().err("illegal addressing mode"))?;
    }

    // some indirect thing?
    if asm.lexer_mut().peek()? == POPEN {
        asm.lexer_mut().eat();
        // jmp and jsr are the only (ABS) and (ABS,X) ops
        if op.0.eq_ignore_ascii_case("JMP") || op.0.eq_ignore_ascii_case("JSR") {
            let expr = expr(asm)?;
            if asm.lexer_mut().peek()? == COMMA {
                asm.lexer_mut().eat();
                expect(asm, UPPERX)?;
                if asm.emit {
                    asm.write(&[op.1.iter().find(|(mode, _)| *mode == IND_ABS_X).unwrap().1])?;
                }
            } else {
                if asm.emit {
                    asm.write(&[op.1.iter().find(|(mode, _)| *mode == IND_ABS).unwrap().1])?;
                }
            }
            expect(asm, PCLOSE)?;
            asm.add_pc(1)?;
            if asm.emit {
                let expr = const_expr(asm, expr)?;
                let word = const_word(asm, expr)?.to_le_bytes();
                asm.write(&word)?;
            }
            asm.add_pc(2)?;
            return Ok(());
        }

        let expr = expr(asm)?;
        // (BP,X) or (D,SP),Y
        if asm.lexer_mut().peek()? == COMMA {
            asm.lexer_mut().eat();
            if asm.lexer_mut().peek()? == IDENT && asm.lexer().string().eq_ignore_ascii_case("SP") {
                let (_, opcode) =
                    op.1.iter()
                        .find(|(mode, _)| *mode == IND_SP)
                        .ok_or_else(|| asm.lexer().err("illegal addressing mode"))?;
                asm.lexer_mut().eat();
                expect(asm, PCLOSE)?;
                expect(asm, COMMA)?;
                expect(asm, UPPERY)?;
                if asm.emit {
                    asm.write(&[*opcode])?;
                }
            } else {
                let (_, opcode) =
                    op.1.iter()
                        .find(|(mode, _)| *mode == IND_X)
                        .ok_or_else(|| asm.lexer().err("illegal addressing mode"))?;
                expect(asm, UPPERX)?;
                expect(asm, PCLOSE)?;
                if asm.emit {
                    asm.write(&[*opcode])?;
                }
            }
            asm.add_pc(1)?;
            if asm.emit {
                let expr = const_expr(asm, expr)?;
                let byte = const_byte(asm, expr)?;
                asm.write(&[byte])?;
            }
            asm.add_pc(1)?;
            return Ok(());
        }

        // (BP),Y or (BP),Z
        expect(asm, PCLOSE)?;
        expect(asm, COMMA)?;
        if asm.lexer_mut().peek()? == UPPERY {
            let (_, opcode) =
                op.1.iter()
                    .find(|(mode, _)| *mode == IND_Y)
                    .ok_or_else(|| asm.lexer().err("illegal addressing mode"))?;
            asm.lexer_mut().eat();
            if asm.emit {
                asm.write(&[*opcode])?;
            }
        } else {
            let (_, opcode) =
                op.1.iter()
                    .find(|(mode, _)| *mode == IND_Z)
                    .ok_or_else(|| asm.lexer().err("illegal addressing mode"))?;
            expect(asm, UPPERZ)?;
            if asm.emit {
                asm.write(&[*opcode])?;
            }
        }
        asm.add_pc(1)?;
        if asm.emit {
            let expr = const_expr(asm, expr)?;
            let byte = const_byte(asm, expr)?;
            asm.write(&[byte])?;
        }
        asm.add_pc(1)?;
        return Ok(());
    }

    // bbs and bbr (these are really just special impl instructions IMO)
    if op.0.eq_ignore_ascii_case("BBS") || op.0.eq_ignore_ascii_case("BBR") {
        let bit = expr(asm)?;
        let bit = const_expr(asm, bit)?;
        if (bit < 0) || (bit > 7) {
            return Err(asm.lexer().err("invalid bit"));
        }
        expect(asm, COMMA)?;

        if let Some((_, (_, opcode))) =
            op.1.iter()
                .enumerate()
                .find(|(i, (mode, _))| (*mode == BP_REL) && (*i == (bit as usize)))
        {
            if asm.emit {
                asm.write(&[*opcode])?;
            }
            asm.add_pc(3)?; // add now so we can compute branch
            {
                let expr = expr(asm)?;
                if asm.emit {
                    let expr = const_expr(asm, expr)?;
                    let byte = const_byte(asm, expr)?;
                    asm.write(&[byte])?;
                }
            }
            expect(asm, COMMA)?;
            {
                let expr = expr(asm)?;
                if asm.emit {
                    let expr = const_expr(asm, expr)?;
                    let branch = const_short_branch(asm, expr)?;
                    asm.write(&[branch])?;
                }
            }
            return Ok(());
        }
        return Err(asm.lexer().err("illegal addressing mode"));
    }

    // rmb and smb (these are really just special impl instructions IMO)
    if op.0.eq_ignore_ascii_case("RMB") || op.0.eq_ignore_ascii_case("SMB") {
        let bit = expr(asm)?;
        let bit = const_expr(asm, bit)?;
        if (bit < 0) || (bit > 7) {
            return Err(asm.lexer().err("invalid bit"));
        }
        expect(asm, COMMA)?;

        if let Some((_, (_, opcode))) =
            op.1.iter()
                .enumerate()
                .find(|(i, (mode, _))| (*mode == BP) && (*i == (bit as usize)))
        {
            if asm.emit {
                asm.write(&[*opcode])?;
            }
            asm.add_pc(3)?; // add now so we can compute branch
            {
                let expr = expr(asm)?;
                if asm.emit {
                    let expr = const_expr(asm, expr)?;
                    let byte = const_byte(asm, expr)?;
                    asm.write(&[byte])?;
                }
            }
            expect(asm, COMMA)?;
            {
                let expr = expr(asm)?;
                if asm.emit {
                    let expr = const_expr(asm, expr)?;
                    let branch = const_short_branch(asm, expr)?;
                    asm.write(&[branch])?;
                }
            }
            return Ok(());
        }
        return Err(asm.lexer().err("illegal addressing mode"));
    }

    // other branching instrs
    if op.0.eq_ignore_ascii_case("BCC")
        || op.0.eq_ignore_ascii_case("BCS")
        || op.0.eq_ignore_ascii_case("BEQ")
        || op.0.eq_ignore_ascii_case("BMI")
        || op.0.eq_ignore_ascii_case("BNE")
        || op.0.eq_ignore_ascii_case("BPL")
        || op.0.eq_ignore_ascii_case("BRU")
        || op.0.eq_ignore_ascii_case("BSR")
        || op.0.eq_ignore_ascii_case("BVC")
        || op.0.eq_ignore_ascii_case("BVS")
    {
        let expr = expr(asm)?;
        // can we optimize the branch into a single byte?
        if let Some(expr) = expr {
            let branch = expr - ((asm.pc() as u32 as i32) + 2); // branch needs +2 (size of instr)
            if (branch >= (i8::MIN as i32))
                && (branch <= (i8::MAX as i32))
                // sad hack. bsr is always word-relative
                && !op.0.eq_ignore_ascii_case("BSR")
            {
                let branch = branch as i8 as u8;
                if asm.emit {
                    asm.write(&[op.1.iter().find(|(mode, _)| *mode == REL).unwrap().1])?;
                }
                if asm.emit {
                    asm.write(&[branch])?;
                }
                asm.add_pc(2)?;
                return Ok(());
            }
        }
        if asm.emit {
            asm.write(&[op.1.iter().find(|(mode, _)| *mode == WREL).unwrap().1])?;
        }
        asm.add_pc(3)?; // ensure we have correct branch
        if asm.emit {
            let expr = const_expr(asm, expr)?;
            let branch = const_long_branch(asm, expr)?.to_le_bytes();
            asm.write(&branch)?;
        }
        return Ok(());
    }

    // BP,X or BP,Y or ABS,X or ABS,Y

    // a leading '|' forces absolute addressing
    let force_abs = asm.lexer_mut().peek()? == PIPE;
    if force_abs {
        asm.lexer_mut().eat();
    }

    let expr = expr(asm)?;

    if asm.lexer_mut().peek()? == COMMA {
        asm.lexer_mut().eat();
        if asm.lexer_mut().peek()? == UPPERX {
            asm.lexer_mut().eat();
            if !force_abs {
                if let Some((_, opcode)) = op.1.iter().find(|(mode, _)| *mode == BP_X) {
                    if let Some(expr) = expr {
                        if (expr as u32) <= (u8::MAX as u32) {
                            if asm.emit {
                                asm.write(&[*opcode])?;
                            }
                            asm.add_pc(1)?;
                            if asm.emit {
                                let byte = expr as u32 as u8;
                                asm.write(&[byte])?;
                            }
                            asm.add_pc(1)?;
                            return Ok(());
                        }
                    }
                }
            }
            let (_, opcode) =
                op.1.iter()
                    .find(|(mode, _)| *mode == ABS_X)
                    .ok_or_else(|| asm.lexer().err("illegal addressing mode"))?;
            if asm.emit {
                asm.write(&[*opcode])?;
            }
            asm.add_pc(1)?;
            if asm.emit {
                let expr = const_expr(asm, expr)?;
                let word = const_word(asm, expr)?.to_le_bytes();
                asm.write(&word)?;
            }
            asm.add_pc(2)?;
            return Ok(());
        }

        if asm.lexer_mut().peek()? == UPPERY {
            asm.lexer_mut().eat();
            if !force_abs {
                if let Some((_, opcode)) = op.1.iter().find(|(mode, _)| *mode == BP_Y) {
                    if let Some(expr) = expr {
                        if (expr as u32) <= (u8::MAX as u32) {
                            if asm.emit {
                                asm.write(&[*opcode])?;
                            }
                            asm.add_pc(1)?;
                            if asm.emit {
                                let byte = expr as u32 as u8;
                                asm.write(&[byte])?;
                            }
                            asm.add_pc(1)?;
                            return Ok(());
                        }
                    }
                }
            }
            let (_, opcode) =
                op.1.iter()
                    .find(|(mode, _)| *mode == ABS_Y)
                    .ok_or_else(|| asm.lexer().err("illegal addressing mode"))?;
            if asm.emit {
                asm.write(&[*opcode])?;
            }
            asm.add_pc(1)?;
            if asm.emit {
                let expr = const_expr(asm, expr)?;
                let word = const_word(asm, expr)?.to_le_bytes();
                asm.write(&word)?;
            }
            asm.add_pc(2)?;
            return Ok(());
        }
        return Err(asm.lexer().err("illegal addressing mode"));
    }

    // BP or ABS
    if !force_abs {
        if let Some((_, opcode)) = op.1.iter().find(|(mode, _)| *mode == BP) {
            if let Some(expr) = expr {
                if (expr as u32) <= (u8::MAX as u32) {
                    if asm.emit {
                        asm.write(&[*opcode])?;
                    }
                    asm.add_pc(1)?;
                    if asm.emit {
                        let byte = expr as u32 as u8;
                        asm.write(&[byte])?;
                    }
                    asm.add_pc(1)?;
                    return Ok(());
                }
            }
        }
    }
    let (_, opcode) =
        op.1.iter()
            .find(|(mode, _)| *mode == ABS)
            .ok_or_else(|| asm.lexer().err("illegal addressing mode"))?;
    if asm.emit {
        asm.write(&[*opcode])?;
    }
    asm.add_pc(1)?;
    if asm.emit {
        let expr = const_expr(asm, expr)?;
        let word = const_word(asm, expr)?.to_le_bytes();
        asm.write(&word)?;
    }
    asm.add_pc(2)?;
    Ok(())
}

struct Asm {
    lexers: Vec<Box<dyn TokenSrc>>,
    output: Box<dyn Write>,
    pc: u16,
    pc_end: bool,
    bss: u16,
    bss_end: bool,
    syms: Vec<(String, i32)>,
    outer_label: String,
    emit: bool,
    bss_mode: bool,
    macros: Vec<Macro>,
}

impl Asm {
    fn new<R: Read + Seek + 'static>(lexer: Lexer<R>, output: Box<dyn Write>) -> Self {
        Self {
            lexers: vec![Box::new(lexer)],
            output,
            pc: 0,
            pc_end: false,
            bss: 0,
            bss_end: false,
            syms: Vec::new(),
            outer_label: String::new(),
            emit: false,
            bss_mode: false,
            macros: Vec::new(),
        }
    }

    fn rewind(&mut self) -> io::Result<()> {
        self.lexers.last_mut().unwrap().rewind()?;
        self.pc = 0;
        self.pc_end = false;
        self.bss = 0;
        self.bss_end = false;
        self.outer_label.clear();
        self.emit = true;
        self.bss_mode = false;
        self.macros.clear();
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.output.write_all(bytes)
    }

    fn lexer(&self) -> &dyn TokenSrc {
        self.lexers.last().unwrap().as_ref()
    }

    fn lexer_mut(&mut self) -> &mut dyn TokenSrc {
        self.lexers.last_mut().unwrap().as_mut()
    }

    fn pc(&self) -> u16 {
        if self.bss_mode {
            self.bss
        } else {
            self.pc
        }
    }

    fn pc_end(&self) -> bool {
        if self.bss_mode {
            self.bss_end
        } else {
            self.pc_end
        }
    }

    fn set_pc_end(&mut self) {
        if self.bss_mode {
            self.bss_end = true;
        } else {
            self.pc_end = true;
        }
    }

    fn set_pc(&mut self, val: u16) {
        if self.bss_mode {
            self.bss = val;
        } else {
            self.pc = val;
        }
    }

    fn add_pc(&mut self, amt: u16) -> io::Result<()> {
        if self.pc_end() && amt > 0 {
            return Err(self.lexer().err("pc overflow"));
        }
        if let Some(value) = self.pc().checked_add(amt) {
            self.set_pc(value);
        } else {
            let value = self.pc().wrapping_add(amt);
            if value > 0 {
                return Err(self.lexer().err("pc overflow"));
            }
            self.set_pc_end();
            self.set_pc(value);
        }
        Ok(())
    }
}

fn const_word(asm: &mut Asm, expr: i32) -> io::Result<u16> {
    if (expr as u32) > (u16::MAX as u32) {
        return Err(asm.lexer().err("expression too large to fit in word"));
    }
    Ok(expr as u16)
}

fn const_byte(asm: &mut Asm, expr: i32) -> io::Result<u8> {
    if (expr as u32) > (u8::MAX as u32) {
        return Err(asm.lexer().err("expression too large to fit in byte"));
    }
    Ok(expr as u8)
}

fn const_short_branch(asm: &mut Asm, expr: i32) -> io::Result<u8> {
    let branch = expr - (asm.pc() as u32 as i32);
    if (branch < (i8::MIN as i32)) || (branch > (i8::MAX as i32)) {
        return Err(asm.lexer().err("branch distance too far"));
    }
    Ok(branch as i8 as u8)
}

fn const_long_branch(asm: &mut Asm, expr: i32) -> io::Result<u16> {
    let branch = expr - (asm.pc() as u32 as i32);
    if (branch < (i16::MIN as i32)) || (branch > (i16::MAX as i32)) {
        return Err(asm.lexer().err("branch distance too far"));
    }
    Ok(branch as i16 as u16)
}

fn end_of_line(asm: &mut Asm) -> io::Result<()> {
    let t = asm.lexer_mut().peek()?;
    match t {
        NEWLINE => {
            asm.lexer_mut().eat();
            Ok(())
        }

        EOF => {
            if asm.lexers.len() > 1 {
                asm.lexers.pop();
            }
            Ok(())
        }

        _ => Err(asm.lexer().err("unexpected garbage")),
    }
}

fn const_expr(asm: &mut Asm, expr: Option<i32>) -> io::Result<i32> {
    expr.ok_or_else(|| asm.lexer().err("expression cannot be resolved"))
}

fn expect(asm: &mut Asm, t: Token) -> io::Result<()> {
    if asm.lexer_mut().peek()? != t {
        return Err(asm.lexer().err("unexpected garbage"));
    }
    asm.lexer_mut().eat();
    Ok(())
}

fn precedence(op: &'static str) -> u8 {
    match op {
        "neg" | "pos" | "lo" | "hi" => 0,
        "/" | "mod" | "*" => 1,
        "asl" | "lsr" | "asr" => 1,
        "+" | "-" | "xor" => 2,
        "not" => 3,
        "and" => 4,
        "or" => 5,
        "(" => 0xFF,
        _ => unreachable!(),
    }
}

fn apply(values: &mut Vec<i32>, op: &'static str) {
    let right = values.pop().unwrap();
    match op {
        "neg" => values.push(-right),
        "pos" => values.push(right),
        "not" => values.push(!right),
        "lo" => values.push(((right as u32) & 0xFF) as i32),
        "hi" => values.push((((right as u32) & 0xFF00) >> 8) as i32),
        "/" => {
            let left = values.pop().unwrap();
            values.push(left / right);
        }
        "mod" => {
            let left = values.pop().unwrap();
            values.push(left % right);
        }
        "*" => {
            let left = values.pop().unwrap();
            values.push(left * right);
        }
        "asl" => {
            let left = values.pop().unwrap();
            values.push(((left as u32) << right) as i32);
        }
        "lsr" => {
            let left = values.pop().unwrap();
            values.push(((left as u32) >> right) as i32);
        }
        "asr" => {
            let left = values.pop().unwrap();
            values.push(left >> right);
        }
        "+" => {
            let left = values.pop().unwrap();
            values.push(left + right);
        }
        "-" => {
            let left = values.pop().unwrap();
            values.push(left - right);
        }
        "xor" => {
            let left = values.pop().unwrap();
            values.push(left ^ right);
        }
        "and" => {
            let left = values.pop().unwrap();
            values.push(left & right);
        }
        "or" => {
            let left = values.pop().unwrap();
            values.push(left | right);
        }
        _ => unreachable!(),
    }
}

fn push_and_apply(values: &mut Vec<i32>, operators: &mut Vec<&'static str>, op: &'static str) {
    while let Some(top) = operators.last() {
        if precedence(&top) > precedence(op) {
            break;
        }
        apply(values, top);
        operators.pop();
    }
    operators.push(op);
}

fn expr(asm: &mut Asm) -> io::Result<Option<i32>> {
    let mut values = Vec::new();
    let mut operators = Vec::new();
    let mut seen_value = false;
    let mut paren_depth = 0;
    let mut unsolved = false;
    loop {
        if asm.lexer_mut().peek()? == STAR {
            asm.lexer_mut().eat();
            if !seen_value {
                values.push(asm.pc() as u32 as i32);
                seen_value = true;
                continue;
            }
            push_and_apply(&mut values, &mut operators, "*");
            seen_value = false;
            continue;
        }
        if asm.lexer_mut().peek()? == PLUS {
            asm.lexer_mut().eat();
            if seen_value {
                push_and_apply(&mut values, &mut operators, "+");
            } else {
                push_and_apply(&mut values, &mut operators, "pos");
            }
            seen_value = false;
            continue;
        }
        if asm.lexer_mut().peek()? == MINUS {
            asm.lexer_mut().eat();
            if seen_value {
                push_and_apply(&mut values, &mut operators, "-");
            } else {
                push_and_apply(&mut values, &mut operators, "neg");
            }
            seen_value = false;
            continue;
        }
        if asm.lexer_mut().peek()? == LESS {
            asm.lexer_mut().eat();
            if seen_value {
                return Err(asm.lexer().err("expected operator"));
            }
            push_and_apply(&mut values, &mut operators, "lo");
            seen_value = false;
            continue;
        }
        if asm.lexer_mut().peek()? == GREATER {
            asm.lexer_mut().eat();
            if seen_value {
                return Err(asm.lexer().err("expected operator"));
            }
            push_and_apply(&mut values, &mut operators, "hi");
            seen_value = false;
            continue;
        }
        if asm.lexer_mut().peek()? == DIV {
            asm.lexer_mut().eat();
            if !seen_value {
                return Err(asm.lexer().err("expected value"));
            }
            push_and_apply(&mut values, &mut operators, "/");
            seen_value = false;
            continue;
        }
        if asm.lexer_mut().peek()? == NUMBER {
            asm.lexer_mut().eat();
            if seen_value {
                return Err(asm.lexer().err("expected operator"));
            }
            values.push(asm.lexer().number());
            seen_value = true;
            continue;
        }
        if asm.lexer_mut().peek()? == POPEN {
            asm.lexer_mut().eat();
            if seen_value {
                return Err(asm.lexer().err("expected operator"));
            }
            paren_depth += 1;
            operators.push("(");
            seen_value = false;
            continue;
        }
        if asm.lexer_mut().peek()? == PCLOSE {
            // this pclose is probably part of the indirect address
            if operators.is_empty() && paren_depth == 0 {
                break;
            }
            asm.lexer_mut().eat();
            paren_depth -= 1;
            if !seen_value {
                return Err(asm.lexer().err("expected value"));
            }
            loop {
                if let Some(op) = operators.pop() {
                    // we apply ops until we see the start of this grouping
                    if op == "(" {
                        break;
                    }
                    apply(&mut values, op);
                } else {
                    return Err(asm.lexer().err("unbalanced parens"));
                }
            }
            continue;
        }
        if asm.lexer_mut().peek()? == IDENT {
            // apply outer label
            if asm.lexer().string().starts_with(".") {
                // todo: remove clone
                let outer_label = asm.outer_label.clone();
                asm.lexer_mut().prepend_string(&outer_label);
            }

            if let Some(sym) = asm
                .syms
                .iter()
                .find(|sym| sym.0.eq_ignore_ascii_case(&asm.lexer().string()))
                .cloned()
            {
                asm.lexer_mut().eat();
                if seen_value {
                    return Err(asm.lexer().err("expected operator"));
                }
                values.push(sym.1);
                seen_value = true;
                continue;
            } else if asm.lexer().string().eq_ignore_ascii_case("mod") {
                asm.lexer_mut().eat();
                if !seen_value {
                    return Err(asm.lexer().err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "mod");
                seen_value = false;
                continue;
            } else if asm.lexer().string().eq_ignore_ascii_case("asl") {
                asm.lexer_mut().eat();
                if !seen_value {
                    return Err(asm.lexer().err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "asl");
                seen_value = false;
                continue;
            } else if asm.lexer().string().eq_ignore_ascii_case("lsr") {
                asm.lexer_mut().eat();
                if !seen_value {
                    return Err(asm.lexer().err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "lsr");
                seen_value = false;
                continue;
            } else if asm.lexer().string().eq_ignore_ascii_case("asr") {
                asm.lexer_mut().eat();
                if !seen_value {
                    return Err(asm.lexer().err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "asr");
                seen_value = false;
                continue;
            } else if asm.lexer().string().eq_ignore_ascii_case("xor") {
                asm.lexer_mut().eat();
                if !seen_value {
                    return Err(asm.lexer().err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "xor");
                seen_value = false;
                continue;
            } else if asm.lexer().string().eq_ignore_ascii_case("and") {
                asm.lexer_mut().eat();
                if !seen_value {
                    return Err(asm.lexer().err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "and");
                seen_value = false;
                continue;
            } else if asm.lexer().string().eq_ignore_ascii_case("or") {
                asm.lexer_mut().eat();
                if !seen_value {
                    return Err(asm.lexer().err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "or");
                seen_value = false;
                continue;
            } else if asm.lexer().string().eq_ignore_ascii_case("not") {
                asm.lexer_mut().eat();
                if seen_value {
                    return Err(asm.lexer().err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "not");
                seen_value = false;
                continue;
            } else {
                // this expression is not solved
                unsolved = true;
                asm.lexer_mut().eat();
                if seen_value {
                    return Err(asm.lexer_mut().err("expected operator"));
                }
                values.push(1);
                seen_value = true;
                continue;
            }
        }

        break;
    }

    while let Some(top) = operators.pop() {
        apply(&mut values, top);
    }

    // we ran into a unsolved label
    if unsolved {
        return Ok(None);
    }

    if let Some(value) = values.pop() {
        Ok(Some(value))
    } else {
        Err(asm.lexer().err("expected value"))
    }
}

fn byt(asm: &mut Asm) -> io::Result<()> {
    loop {
        if asm.lexer_mut().peek()? == STRING {
            if asm.emit {
                // todo: remove clone
                let string = asm.lexer().string().to_string();
                asm.write(string.as_bytes())?;
            }
            asm.add_pc(asm.lexer().string().len() as u16)?;
            asm.lexer_mut().eat();
        } else {
            let expr = expr(asm)?;
            if asm.emit {
                let expr = const_expr(asm, expr)?;
                let byte = const_byte(asm, expr)?;
                asm.write(&[byte])?;
            }
            asm.add_pc(1)?;
        }
        if asm.lexer_mut().peek()? != COMMA {
            break;
        }
        asm.lexer_mut().eat();
    }
    end_of_line(asm)?;
    Ok(())
}

fn wrd(asm: &mut Asm) -> io::Result<()> {
    loop {
        let expr = expr(asm)?;
        if asm.emit {
            let expr = const_expr(asm, expr)?;
            let word = &const_word(asm, expr)?.to_le_bytes();
            asm.write(word)?;
        }
        asm.add_pc(2)?;
        if asm.lexer_mut().peek()? != COMMA {
            break;
        }
        asm.lexer_mut().eat();
    }
    end_of_line(asm)?;
    Ok(())
}

fn pad(asm: &mut Asm) -> io::Result<()> {
    let expr = expr(asm)?;
    let expr = const_expr(asm, expr)?;
    let word = const_word(asm, expr)?;
    if asm.emit && !asm.bss_mode {
        for _ in 0..word {
            asm.write(&[0xEA])?;
        }
    }
    asm.add_pc(word)?;
    end_of_line(asm)?;
    Ok(())
}

fn adj(asm: &mut Asm) -> io::Result<()> {
    let expr = expr(asm)?;
    let expr = const_expr(asm, expr)?;
    let word = const_word(asm, expr)?;
    let adj = asm.pc() % word;
    if asm.emit {
        for _ in 0..adj {
            asm.write(&[0xEA])?;
        }
    }
    asm.add_pc(adj)?;
    end_of_line(asm)?;
    Ok(())
}

fn bss(asm: &mut Asm) -> io::Result<()> {
    asm.bss_mode = true;
    end_of_line(asm)?;
    Ok(())
}

fn txt(asm: &mut Asm) -> io::Result<()> {
    asm.bss_mode = false;
    end_of_line(asm)?;
    Ok(())
}

fn inf(asm: &mut Asm) -> io::Result<()> {
    if asm.lexer_mut().peek()? != STRING {
        return Err(asm.lexer().err("expected file name"));
    }
    let file = File::open(&asm.lexer().string())?;
    asm.lexer_mut().eat();
    let reader = Reader::new(file);
    let lexer = Lexer::new(reader);
    asm.lexers.push(Box::new(lexer));
    Ok(())
}

fn mac(asm: &mut Asm, name: String) -> io::Result<()> {
    end_of_line(asm)?;
    let mut tokens = Vec::new();
    let mut strings = Vec::new();
    loop {
        if (asm.lexer_mut().peek()? == IDENT) && asm.lexer().string().eq_ignore_ascii_case("EMC") {
            asm.lexer_mut().eat();
            tokens.push(MacroTokenOrArgument::Token(MacroToken {
                inner: EOF,
                string_index: 0,
                number: 0,
                line: asm.lexer().line(),
            }));
            break;
        }
        match asm.lexer_mut().peek()? {
            EOF => return Err(asm.lexer().err("unexpected end of file"))?,
            tok @ (IDENT | STRING) => {
                tokens.push(MacroTokenOrArgument::Token(MacroToken {
                    inner: tok,
                    string_index: strings.len(),
                    number: 0,
                    line: asm.lexer().line(),
                }));
                strings.push(asm.lexer().string().to_string());
            }
            NUMBER => {
                tokens.push(MacroTokenOrArgument::Token(MacroToken {
                    inner: NUMBER,
                    string_index: 0,
                    number: asm.lexer().number(),
                    line: asm.lexer().line(),
                }));
            }
            ARGUMENT => {
                let index = asm.lexer().number();
                if index < 1 {
                    return Err(asm
                        .lexer()
                        .err("macro argument index must be greater than 0"))?;
                }
                tokens.push(MacroTokenOrArgument::Argument {
                    index: (index as usize) - 1,
                    line: asm.lexer().line(),
                });
            }
            tok => tokens.push(MacroTokenOrArgument::Token(MacroToken {
                inner: tok,
                string_index: 0,
                number: 0,
                line: asm.lexer().line(),
            })),
        }
        asm.lexer_mut().eat();
    }
    asm.macros.push(Macro {
        name,
        tokens,
        strings,
    });
    Ok(())
}

type POp = (&'static str, fn(&mut Asm) -> io::Result<()>);

#[rustfmt::skip]
const POPS: &[POp] = &[
    ("BYT", byt),
    ("WRD", wrd),
    ("PAD", pad),
    ("ADJ", adj),
    ("BSS", bss),
    ("TXT", txt),
    ("INF", inf),
    //("IFF", iff),
];

type Token = u16;

const NEWLINE: Token = b'\n' as u16;
const STAR: Token = b'*' as u16;
const COMMA: Token = b',' as u16;
const HASH: Token = b'#' as u16;
const UPPERA: Token = b'A' as u16;
const UPPERX: Token = b'X' as u16;
const UPPERY: Token = b'Y' as u16;
const UPPERZ: Token = b'Z' as u16;
const PIPE: Token = b'|' as u16;
const POPEN: Token = b'(' as u16;
const PCLOSE: Token = b')' as u16;
const LESS: Token = b'<' as u16;
const GREATER: Token = b'>' as u16;
const PLUS: Token = b'+' as u16;
const MINUS: Token = b'-' as u16;
const DIV: Token = b'/' as u16;
const EOF: Token = 0x8000;
const IDENT: Token = 0x8001;
const NUMBER: Token = 0x8002;
const STRING: Token = 0x8003;
const ARGUMENT: Token = 0x8004;

trait TokenSrc {
    fn rewind(&mut self) -> io::Result<()>;

    fn err(&self, msg: &str) -> io::Error;

    fn peek(&mut self) -> io::Result<Token>;

    fn eat(&mut self);

    fn string(&self) -> &str;

    fn prepend_string(&mut self, string: &str);

    fn number(&self) -> i32;

    fn line(&self) -> usize;
}

struct Lexer<R> {
    inner: Reader<R>,
    string: String,
    number: i32,
    stash: Option<Token>,
    line: usize,
}

impl<R: Read + Seek> Lexer<R> {
    fn new(inner: Reader<R>) -> Self {
        Self {
            inner,
            string: String::new(),
            number: 0,
            stash: None,
            line: 1,
        }
    }
}

impl<R: Read + Seek> TokenSrc for Lexer<R> {
    fn rewind(&mut self) -> io::Result<()> {
        self.inner.rewind()?;
        self.string.clear();
        self.number = 0;
        self.stash = None;
        self.line = 1;
        Ok(())
    }

    fn err(&self, msg: &str) -> io::Error {
        io::Error::new(ErrorKind::InvalidData, format!("{}: {msg}", self.line))
    }

    fn peek(&mut self) -> io::Result<Token> {
        if let Some(t) = self.stash {
            return Ok(t);
        }

        // skip whitespace
        while let Some(c) = self.inner.peek()? {
            if !b" \t\r".contains(&c) {
                break;
            }
            self.inner.eat();
        }
        // skip comment
        if let Some(b';') = self.inner.peek()? {
            while !matches!(self.inner.peek()?, Some(b'\n')) {
                self.inner.eat();
            }
        }

        if let Some(c) = self.inner.peek()? {
            // argument
            if c == b'?' {
                self.inner.eat();
                while let Some(c) = self.inner.peek()? {
                    if !c.is_ascii_digit() {
                        break;
                    }
                    self.string.push(c as char);
                    self.inner.eat();
                }
                self.number =
                    i32::from_str_radix(&self.string, 10).map_err(|e| self.err(&e.to_string()))?;
                self.stash = Some(ARGUMENT);
                return Ok(ARGUMENT);
            }

            // number
            if c.is_ascii_digit() || c == b'$' || c == b'%' {
                let radix = match c {
                    b'$' => {
                        self.inner.eat();
                        16
                    }
                    b'%' => {
                        self.inner.eat();
                        2
                    }
                    _ => 10,
                };
                while let Some(c) = self.inner.peek()? {
                    if !c.is_ascii_alphanumeric() {
                        break;
                    }
                    self.string.push(c as char);
                    self.inner.eat();
                }
                self.number = i32::from_str_radix(&self.string, radix)
                    .map_err(|e| self.err(&e.to_string()))?;
                self.stash = Some(NUMBER);
                return Ok(NUMBER);
            }

            // string
            if c == b'"' {
                self.inner.eat();
                while let Some(c) = self.inner.peek()? {
                    if c == b'"' {
                        self.inner.eat();
                        break;
                    }
                    self.string.push(c as char);
                    self.inner.eat();
                }
                self.stash = Some(STRING);
                return Ok(STRING);
            }

            // char
            if c == b'\'' {
                self.inner.eat();
                if let Some(c) = self.inner.peek()? {
                    if c.is_ascii_graphic() {
                        self.inner.eat();
                        self.number = c as i32;
                        self.stash = Some(NUMBER);
                        return Ok(NUMBER);
                    }
                }
                return Err(self.err("unexpected garbage"));
            }

            // idents and single chars
            while let Some(c) = self.inner.peek()? {
                if !c.is_ascii_alphanumeric() && !b"_.".contains(&c) {
                    break;
                }
                self.inner.eat();
                self.string.push(c as char);
            }
            if self.string.len() > 1 {
                if self.string.len() > 16 {
                    return Err(self.err("label too long"));
                }
                self.stash = Some(IDENT);
                return Ok(IDENT);
            }
            // the char wasn't an ident, so wasnt eaten
            if self.string.len() == 0 {
                self.inner.eat();
            }
            self.stash = Some(c.to_ascii_uppercase() as u16);
            return Ok(c.to_ascii_uppercase() as u16);
        }

        self.inner.eat();
        self.stash = Some(EOF);
        Ok(EOF)
    }

    fn eat(&mut self) {
        self.string.clear();
        if let Some(NEWLINE) = self.stash.take() {
            self.line += 1;
        }
    }

    fn string(&self) -> &str {
        &self.string
    }

    fn prepend_string(&mut self, string: &str) {
        self.string.insert_str(0, string);
    }

    fn number(&self) -> i32 {
        self.number
    }

    fn line(&self) -> usize {
        self.line
    }
}

#[derive(Clone)]
struct MacroToken {
    inner: Token,
    string_index: usize,
    number: i32,
    line: usize,
}

#[derive(Clone)]
enum MacroTokenOrArgument {
    Token(MacroToken),
    Argument { index: usize, line: usize },
}

#[derive(Clone)]
struct Macro {
    name: String,
    tokens: Vec<MacroTokenOrArgument>,
    strings: Vec<String>,
}

struct MacroInvocation {
    inner: Macro,
    invocation_line: usize,
    pos: usize,
    string: String,
    args: Vec<MacroToken>,
    arg_strings: Vec<String>,
}

impl TokenSrc for MacroInvocation {
    fn rewind(&mut self) -> io::Result<()> {
        self.pos = 0;
        Ok(())
    }

    fn err(&self, msg: &str) -> io::Error {
        io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "{}:{}:{}: {msg}",
                self.invocation_line,
                self.inner.name,
                match &self.inner.tokens[self.pos] {
                    MacroTokenOrArgument::Token(tok) => tok.line,
                    MacroTokenOrArgument::Argument { line, .. } => *line,
                }
            ),
        )
    }

    fn peek(&mut self) -> io::Result<Token> {
        match &self.inner.tokens[self.pos] {
            MacroTokenOrArgument::Token(tok) if (tok.inner == STRING) || (tok.inner == IDENT) => {
                self.string.clear();
                // todo: remove clone
                self.string = self.inner.strings[tok.string_index].clone();
                Ok(tok.inner)
            }
            MacroTokenOrArgument::Token(tok) => Ok(tok.inner),
            MacroTokenOrArgument::Argument { index, .. } => {
                if *index >= self.args.len() {
                    return Err(self.err("argument is undefined"));
                }
                let tok = &self.args[*index];
                if (tok.inner == STRING) || (tok.inner == IDENT) {
                    self.string.clear();
                    // todo: remove clone
                    self.string = self.arg_strings[self.args[*index].string_index].clone();
                }
                Ok(tok.inner)
            }
        }
    }

    fn eat(&mut self) {
        self.pos += 1;
    }

    fn string(&self) -> &str {
        &self.string
    }

    fn prepend_string(&mut self, string: &str) {
        self.string.insert_str(0, string);
    }

    fn number(&self) -> i32 {
        match &self.inner.tokens[self.pos] {
            MacroTokenOrArgument::Token(tok) => tok.number,
            MacroTokenOrArgument::Argument { index, .. } => self.args[*index].number,
        }
    }

    fn line(&self) -> usize {
        match &self.inner.tokens[self.pos] {
            MacroTokenOrArgument::Token(tok) => tok.line,
            MacroTokenOrArgument::Argument { index, .. } => self.args[*index].line,
        }
    }
}

struct Reader<R> {
    inner: R,
    stash: Option<u8>,
}

impl<R: Read + Seek> Reader<R> {
    fn new(inner: R) -> Self {
        Self { inner, stash: None }
    }

    fn rewind(&mut self) -> io::Result<()> {
        self.inner.rewind()?;
        self.stash = None;
        Ok(())
    }

    fn peek(&mut self) -> io::Result<Option<u8>> {
        if let Some(c) = self.stash {
            return Ok(Some(c));
        }
        let mut buf = [0];
        self.stash = self
            .inner
            .read(&mut buf)
            .map(|n| if n == 0 { None } else { Some(buf[0]) })?;
        Ok(self.stash)
    }

    fn eat(&mut self) -> Option<u8> {
        self.stash.take()
    }
}

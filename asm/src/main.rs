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

    eprint!("pass2: ");
    let mut asm = asm.rewind()?;
    pass(&mut asm)?;
    eprintln!("ok");
    Ok(())
}

fn pass(asm: &mut Asm<File>) -> Result<(), Box<dyn Error>> {
    loop {
        if asm.lexer.peek()? == EOF {
            break;
        }

        // special case, setting PC
        if asm.lexer.peek()? == STAR {
            asm.lexer.eat();
            if asm.lexer.peek()? != IDENT && !asm.lexer.string.eq_ignore_ascii_case("EQU") {
                Err(asm.lexer.err("expected EQU"))?;
            }
            asm.lexer.eat();
            let expr = expr(asm)?;
            let expr = const_expr(asm, expr)?;
            asm.pc = const_word(asm, expr)?;
            end_of_line(asm)?;
            continue;
        }

        // label?
        if asm.lexer.peek()? == IDENT
            && !asm.lexer.string.eq_ignore_ascii_case("EQU")
            && !OPS
                .iter()
                .any(|op| asm.lexer.string.eq_ignore_ascii_case(op.0))
            && !POPS
                .iter()
                .any(|op| asm.lexer.string.eq_ignore_ascii_case(op.0))
        {
            // apply outer label
            if asm.lexer.string.starts_with(".") {
                asm.lexer.string = format!("{}{}", asm.outer_label, asm.lexer.string);
            } else {
                asm.outer_label.clear();
                asm.outer_label.push_str(&asm.lexer.string);
            }

            // is this already in the symbol table?
            let sym_index = if let Some(item) = asm
                .syms
                .iter()
                .enumerate()
                .find(|item| asm.lexer.string == item.1 .0)
            {
                // allowed to redef during second pass
                // todo: should test if label value didnt change
                if !asm.emit {
                    Err(asm.lexer.err("symbol already defined"))?
                }
                item.0
            } else {
                // save the label in the symbol table
                let index = asm.syms.len();
                asm.syms.push((asm.lexer.string.clone(), 0));
                index
            };
            asm.lexer.eat();

            // check if this label is being defined to a value
            if asm.lexer.peek()? == IDENT && asm.lexer.string.eq_ignore_ascii_case("EQU") {
                asm.lexer.eat();
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
            asm.syms[sym_index].1 = asm.pc as u32 as i32;
        }

        // pseudo op?
        if asm.lexer.peek()? == IDENT {
            if let Some(pop) = POPS
                .iter()
                .find(|pop| asm.lexer.string.eq_ignore_ascii_case(pop.0))
            {
                asm.lexer.eat();
                // evaluate the pseudo op
                pop.1(asm)?;
                end_of_line(asm)?;
                continue;
            }
        }

        // op?
        if asm.lexer.peek()? == IDENT {
            let op = OPS
                .iter()
                .find(|op| asm.lexer.string.eq_ignore_ascii_case(op.0))
                .ok_or_else(|| asm.lexer.err("unknown opcode"))?;
            asm.lexer.eat();
            operand(asm, op)?;
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

    ("LDA", &[(IMM, 0xA9), (ABS, 0xAD), (BP, 0xA5), (IND_X, 0xA1), (IND_Y, 0xB1), (IND_Z, 0xB2), (IND_SP, 0xE2), (BP_X, 0xB5), (ABS_X, 0xBD), (ABS_Y, 0xB9)]),
    ("STA", &[(ABS, 0x8D), (BP, 0x85), (IND_X, 0x81), (IND_Y, 0x91), (IND_Z, 0x92), (IND_SP, 0x82), (BP_X, 0x95), (ABS_X, 0x9D), (ABS_Y, 0x99)]),

    ("JMP", &[(ABS, 0x4C), (IND_ABS, 0x6C), (IND_ABS_X, 0x7C)]),
    ("JSR", &[(ABS, 0x20), (IND_ABS, 0x22), (IND_ABS_X, 0x23)]),
];

fn operand<R: Read + Seek>(asm: &mut Asm<R>, op: &Op) -> io::Result<()> {
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
    if asm.lexer.peek()? == HASH {
        asm.lexer.eat();
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
        return Err(asm.lexer.err("illegal addressing mode"));
    }

    // accum?
    if asm.lexer.peek()? == UPPERA {
        asm.lexer.eat();
        if let Some((_, opcode)) = op.1.iter().find(|(mode, _)| *mode == ACCUM) {
            if asm.emit {
                asm.write(&[*opcode])?;
            }
            asm.add_pc(1)?;
            return Ok(());
        }
        return Err(asm.lexer.err("illegal addressing mode"))?;
    }

    // some indirect thing?
    if asm.lexer.peek()? == POPEN {
        asm.lexer.eat();
        // jmp and jsr are the only (ABS) and (ABS,X) ops
        if op.0.eq_ignore_ascii_case("JMP") || op.0.eq_ignore_ascii_case("JSR") {
            let expr = expr(asm)?;
            if asm.lexer.peek()? == COMMA {
                asm.lexer.eat();
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
        if asm.lexer.peek()? == COMMA {
            asm.lexer.eat();
            if asm.lexer.peek()? == IDENT && asm.lexer.string.eq_ignore_ascii_case("SP") {
                let (_, opcode) =
                    op.1.iter()
                        .find(|(mode, _)| *mode == IND_SP)
                        .ok_or_else(|| asm.lexer.err("illegal addressing mode"))?;
                asm.lexer.eat();
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
                        .ok_or_else(|| asm.lexer.err("illegal addressing mode"))?;
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
        if asm.lexer.peek()? == UPPERY {
            let (_, opcode) =
                op.1.iter()
                    .find(|(mode, _)| *mode == IND_Y)
                    .ok_or_else(|| asm.lexer.err("illegal addressing mode"))?;
            asm.lexer.eat();
            if asm.emit {
                asm.write(&[*opcode])?;
            }
        } else {
            let (_, opcode) =
                op.1.iter()
                    .find(|(mode, _)| *mode == IND_Z)
                    .ok_or_else(|| asm.lexer.err("illegal addressing mode"))?;
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
            return Err(asm.lexer.err("invalid bit"));
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
        return Err(asm.lexer.err("illegal addressing mode"));
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
            let branch = expr - ((asm.pc as u32 as i32) + 2); // branch needs +2 (size of instr)
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
    let force_abs = asm.lexer.peek()? == PIPE;
    if force_abs {
        asm.lexer.eat();
    }

    let expr = expr(asm)?;

    if asm.lexer.peek()? == COMMA {
        asm.lexer.eat();
        if asm.lexer.peek()? == UPPERX {
            asm.lexer.eat();
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
                    .ok_or_else(|| asm.lexer.err("illegal addressing mode"))?;
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

        if asm.lexer.peek()? == UPPERY {
            asm.lexer.eat();
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
                    .ok_or_else(|| asm.lexer.err("illegal addressing mode"))?;
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
        return Err(asm.lexer.err("illegal addressing mode"));
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
            .ok_or_else(|| asm.lexer.err("illegal addressing mode"))?;
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

struct Asm<R> {
    lexer: Lexer<R>,
    output: Box<dyn Write>,
    pc: u16,
    pc_end: bool,
    syms: Vec<(String, i32)>,
    outer_label: String,
    emit: bool,
}

impl<R: Read + Seek> Asm<R> {
    fn new(lexer: Lexer<R>, output: Box<dyn Write>) -> Self {
        Self {
            lexer,
            output,
            pc: 0,
            pc_end: false,
            syms: Vec::new(),
            outer_label: String::new(),
            emit: false,
        }
    }

    fn rewind(self) -> io::Result<Self> {
        let Self {
            lexer,
            output,
            syms,
            ..
        } = self;
        let lexer = lexer.rewind()?;
        Ok(Self {
            lexer,
            output,
            pc: 0,
            pc_end: false,
            syms,
            outer_label: String::new(),
            emit: true,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.output.write_all(bytes)
    }

    fn add_pc(&mut self, amt: u16) -> io::Result<()> {
        if self.pc_end && amt > 0 {
            return Err(self.lexer.err("pc overflow"));
        }
        if let Some(value) = self.pc.checked_add(amt) {
            self.pc = value;
        } else {
            let value = self.pc.wrapping_add(amt);
            if value > 0 {
                return Err(self.lexer.err("pc overflow"));
            }
            self.pc_end = true;
            self.pc = value;
        }
        Ok(())
    }
}

fn const_word<R: Read + Seek>(asm: &mut Asm<R>, expr: i32) -> io::Result<u16> {
    if (expr as u32) > (u16::MAX as u32) {
        return Err(asm.lexer.err("expression too large to fit in word"));
    }
    Ok(expr as u16)
}

fn const_byte<R: Read + Seek>(asm: &mut Asm<R>, expr: i32) -> io::Result<u8> {
    if (expr as u32) > (u8::MAX as u32) {
        return Err(asm.lexer.err("expression too large to fit in byte"));
    }
    Ok(expr as u8)
}

fn const_short_branch<R: Read + Seek>(asm: &mut Asm<R>, expr: i32) -> io::Result<u8> {
    let branch = expr - (asm.pc as u32 as i32);
    if (branch < (i8::MIN as i32)) || (branch > (i8::MAX as i32)) {
        return Err(asm.lexer.err("branch distance too far"));
    }
    Ok(branch as i8 as u8)
}

fn const_long_branch<R: Read + Seek>(asm: &mut Asm<R>, expr: i32) -> io::Result<u16> {
    let branch = expr - (asm.pc as u32 as i32);
    if (branch < (i16::MIN as i32)) || (branch > (i16::MAX as i32)) {
        return Err(asm.lexer.err("branch distance too far"));
    }
    Ok(branch as i16 as u16)
}

fn end_of_line<R: Read + Seek>(asm: &mut Asm<R>) -> io::Result<()> {
    let t = asm.lexer.peek()?;
    if t == NEWLINE || t == EOF {
        asm.lexer.eat();
        return Ok(());
    }
    Err(asm.lexer.err("unexpected garbage"))
}

fn const_expr<R: Read + Seek>(asm: &mut Asm<R>, expr: Option<i32>) -> io::Result<i32> {
    expr.ok_or_else(|| asm.lexer.err("expression cannot be resolved"))
}

fn expect<R: Read + Seek>(asm: &mut Asm<R>, t: Token) -> io::Result<()> {
    if asm.lexer.peek()? != t {
        return Err(asm.lexer.err("unexpected garbage"));
    }
    asm.lexer.eat();
    Ok(())
}

fn precedence(op: &'static str) -> u8 {
    match op {
        "neg" | "lo" | "hi" => 0,
        "/" | "mod" | "*" => 1,
        "asl" | "lsr" | "asr" => 1,
        "+" | "-" | "xor" => 2,
        "not" => 3,
        "and" => 4,
        "or" => 5,
        _ => unreachable!(),
    }
}

fn apply(values: &mut Vec<i32>, op: &'static str) {
    let right = values.pop().unwrap();
    match op {
        "neg" => values.push(-right),
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
    }
    operators.push(op);
}

fn expr<R: Read + Seek>(asm: &mut Asm<R>) -> io::Result<Option<i32>> {
    let mut values = Vec::new();
    let mut operators = Vec::new();
    let mut seen_value = false;
    let mut paren_balance = 0;
    let mut unsolved = false;
    loop {
        if asm.lexer.peek()? == STAR {
            asm.lexer.eat();
            if !seen_value {
                values.push(asm.pc as u32 as i32);
                seen_value = true;
                continue;
            }
            push_and_apply(&mut values, &mut operators, "*");
            seen_value = false;
            continue;
        }
        if asm.lexer.peek()? == PLUS {
            asm.lexer.eat();
            if !seen_value {
                return Err(asm.lexer.err("expected value"));
            }
            push_and_apply(&mut values, &mut operators, "+");
            seen_value = false;
            continue;
        }
        if asm.lexer.peek()? == MINUS {
            asm.lexer.eat();
            if seen_value {
                push_and_apply(&mut values, &mut operators, "-");
            } else {
                push_and_apply(&mut values, &mut operators, "neg");
            }
            seen_value = false;
            continue;
        }
        if asm.lexer.peek()? == LESS {
            asm.lexer.eat();
            if seen_value {
                return Err(asm.lexer.err("expected value"));
            }
            push_and_apply(&mut values, &mut operators, "lo");
            seen_value = false;
            continue;
        }
        if asm.lexer.peek()? == GREATER {
            asm.lexer.eat();
            if seen_value {
                return Err(asm.lexer.err("expected value"));
            }
            push_and_apply(&mut values, &mut operators, "hi");
            seen_value = false;
            continue;
        }
        if asm.lexer.peek()? == DIV {
            asm.lexer.eat();
            if !seen_value {
                return Err(asm.lexer.err("expected value"));
            }
            push_and_apply(&mut values, &mut operators, "/");
            seen_value = false;
            continue;
        }
        if asm.lexer.peek()? == NUMBER {
            asm.lexer.eat();
            if seen_value {
                return Err(asm.lexer.err("expected operator"));
            }
            values.push(asm.lexer.number);
            seen_value = true;
            continue;
        }
        if asm.lexer.peek()? == POPEN {
            asm.lexer.eat();
            if seen_value {
                return Err(asm.lexer.err("expected operator"));
            }
            paren_balance += 1;
            operators.push("(");
            seen_value = false;
            continue;
        }
        if asm.lexer.peek()? == PCLOSE {
            // this pclose is probably part of the indirect address
            if operators.is_empty() && paren_balance == 0 {
                break;
            }
            asm.lexer.eat();
            if !seen_value {
                return Err(asm.lexer.err("expected value"));
            }
            loop {
                if let Some(op) = operators.pop() {
                    if op == ")" {
                        break;
                    }
                    apply(&mut values, op);
                } else {
                    return Err(asm.lexer.err("unbalanced parens"));
                }
            }
            continue;
        }
        if asm.lexer.peek()? == IDENT {
            // apply outer label
            if asm.lexer.string.starts_with(".") {
                asm.lexer.string = format!("{}{}", asm.outer_label, asm.lexer.string);
            }

            if let Some(sym) = asm
                .syms
                .iter()
                .find(|sym| sym.0.eq_ignore_ascii_case(&asm.lexer.string))
            {
                asm.lexer.eat();
                if seen_value {
                    return Err(asm.lexer.err("expected operator"));
                }
                values.push(sym.1);
                seen_value = true;
                continue;
            } else if asm.lexer.string.eq_ignore_ascii_case("mod") {
                asm.lexer.eat();
                if !seen_value {
                    return Err(asm.lexer.err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "mod");
                seen_value = false;
                continue;
            } else if asm.lexer.string.eq_ignore_ascii_case("asl") {
                asm.lexer.eat();
                if !seen_value {
                    return Err(asm.lexer.err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "asl");
                seen_value = false;
                continue;
            } else if asm.lexer.string.eq_ignore_ascii_case("lsr") {
                asm.lexer.eat();
                if !seen_value {
                    return Err(asm.lexer.err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "lsr");
                seen_value = false;
                continue;
            } else if asm.lexer.string.eq_ignore_ascii_case("asr") {
                asm.lexer.eat();
                if !seen_value {
                    return Err(asm.lexer.err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "asr");
                seen_value = false;
                continue;
            } else if asm.lexer.string.eq_ignore_ascii_case("xor") {
                asm.lexer.eat();
                if !seen_value {
                    return Err(asm.lexer.err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "xor");
                seen_value = false;
                continue;
            } else if asm.lexer.string.eq_ignore_ascii_case("and") {
                asm.lexer.eat();
                if !seen_value {
                    return Err(asm.lexer.err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "and");
                seen_value = false;
                continue;
            } else if asm.lexer.string.eq_ignore_ascii_case("or") {
                asm.lexer.eat();
                if !seen_value {
                    return Err(asm.lexer.err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "or");
                seen_value = false;
                continue;
            } else if asm.lexer.string.eq_ignore_ascii_case("not") {
                asm.lexer.eat();
                if seen_value {
                    return Err(asm.lexer.err("expected value"));
                }
                push_and_apply(&mut values, &mut operators, "not");
                seen_value = false;
                continue;
            } else {
                // this expression is not solved
                unsolved = true;
                asm.lexer.eat();
                if seen_value {
                    return Err(asm.lexer.err("expected operator"));
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
        Err(asm.lexer.err("expected value"))
    }
}

fn bytes<R: Read + Seek>(asm: &mut Asm<R>) -> io::Result<()> {
    loop {
        if asm.lexer.peek()? == STRING {
            if asm.emit {
                // todo: hacky, make method to write the current string buffer
                let bytes = asm.lexer.string.clone();
                asm.write(bytes.as_bytes())?;
            }
            asm.add_pc(asm.lexer.string.len() as u16)?;
            asm.lexer.eat();
        } else {
            let expr = expr(asm)?;
            if asm.emit {
                let expr = const_expr(asm, expr)?;
                let byte = const_byte(asm, expr)?;
                asm.write(&[byte])?;
            }
            asm.add_pc(1)?;
        }
        if asm.lexer.peek()? != COMMA {
            break;
        }
        asm.lexer.eat();
    }
    Ok(())
}

fn words<R: Read + Seek>(asm: &mut Asm<R>) -> io::Result<()> {
    loop {
        let expr = expr(asm)?;
        if asm.emit {
            let expr = const_expr(asm, expr)?;
            let word = &const_word(asm, expr)?.to_le_bytes();
            asm.write(word)?;
        }
        asm.add_pc(2)?;
        if asm.lexer.peek()? != COMMA {
            break;
        }
        asm.lexer.eat();
    }
    Ok(())
}

fn pad<R: Read + Seek>(asm: &mut Asm<R>) -> io::Result<()> {
    let expr = expr(asm)?;
    let expr = const_expr(asm, expr)?;
    let word = const_word(asm, expr)?;
    if asm.emit {
        for _ in 0..word {
            asm.write(&[0xEA])?;
        }
    }
    asm.add_pc(word)?;
    Ok(())
}

type POp = (&'static str, fn(&mut Asm<File>) -> io::Result<()>);

#[rustfmt::skip]
const POPS: &[POp] = &[
    ("BYT", bytes),
    ("WRD", words),
    ("PAD", pad),
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

    fn rewind(self) -> io::Result<Self> {
        let Self { inner, .. } = self;
        Ok(Self {
            inner: inner.rewind()?,
            string: String::new(),
            number: 0,
            stash: None,
            line: 1,
        })
    }

    fn err<E>(&self, e: E) -> io::Error
    where
        E: Into<Box<dyn Error + Send + Sync>>,
    {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("{}: {}", self.line, e.into()),
        )
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
                self.number = i32::from_str_radix(&self.string, radix).map_err(|e| self.err(e))?;
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

    fn eat(&mut self) -> Token {
        self.string.clear();
        match self.stash.take() {
            Some(NEWLINE) => {
                self.line += 1;
                NEWLINE
            }
            Some(t) => t,
            None => {
                // once we hit EOF, it is forever
                EOF
            }
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

    fn rewind(self) -> io::Result<Self> {
        let Self { mut inner, .. } = self;
        inner.rewind()?;
        Ok(Self { inner, stash: None })
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

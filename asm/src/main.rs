use std::{
    error::Error,
    fs::File,
    io::{self, ErrorKind, Read},
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
    let mut asm = Asm::new(lexer);

    loop {
        if asm.lexer.peek()? == EOF {
            break;
        }

        // label?
        if asm.lexer.peek()? == IDENT
            && !OPS
                .iter()
                .any(|op| asm.lexer.string.eq_ignore_ascii_case(op.0))
        {
            // save the label in the symbol table
            let sym_index = asm.syms.len();
            asm.syms.push((asm.lexer.string.clone(), 0));
            asm.lexer.eat();

            // check if this label is being defined to a value
            if asm.lexer.peek()? == EQU {
                asm.lexer.eat();
                if asm.emit {
                    asm.syms[sym_index].1 = const_expr(&mut asm)?;
                } else if let Some(expr) = expr(&mut asm)? {
                    asm.syms[sym_index].1 = expr;
                } else {
                    // we couldn't evaluate this yet, so remove it
                    asm.syms.pop();
                }
                end_of_line(&mut asm)?;
                continue;
            }
            // otherwise it is a pointer to the current PC
            asm.syms[sym_index].1 = asm.pc as u32 as i32;
        }

        // pseudo op?
        if asm.lexer.peek()? == PSEUDO {
            let pop = POPS
                .iter()
                .find(|pop| asm.lexer.string.eq_ignore_ascii_case(pop.0))
                .ok_or_else(|| asm.lexer.err("unknown pseudo opcode"))?;
            asm.lexer.eat();
            // evaluate the pseudo op
            pop.1(&mut asm)?;
            end_of_line(&mut asm)?;
            continue;
        }

        // op?
        if asm.lexer.peek()? == IDENT {
            let op = OPS
                .iter()
                .find(|op| asm.lexer.string.eq_ignore_ascii_case(op.0))
                .ok_or_else(|| asm.lexer.err("unknown opcode"))?;
            asm.lexer.eat();
        }

        end_of_line(&mut asm)?;
    }

    Ok(())
}

const IMM: u32 = 1 << 0;
const ABS: u32 = 1 << 1;
const BP: u32 = 1 << 2;
const ACCUM: u32 = 1 << 3;
const IMPL: u32 = 1 << 4;
const IND_X: u32 = 1 << 5; // (BP,X)
const IND_Y: u32 = 1 << 6; // (BP),Y
const IND_Z: u32 = 1 << 7; // (BP),Z
const IND_SP: u32 = 1 << 8; // (d,SP),Y
const BP_X: u32 = 1 << 9;
const BP_Y: u32 = 1 << 10;
const ABS_X: u32 = 1 << 11;
const ABS_Y: u32 = 1 << 12;
const REL: u32 = 1 << 13;
const WREL: u32 = 1 << 14;
const IND_ABS: u32 = 1 << 15; // (ABS)
const BP_REL: u32 = 1 << 16;
const IND_ABS_X: u32 = 1 << 17; // (ABS,X)

type Op = (&'static str, u32, &'static [u8]);

#[rustfmt::skip]
const OPS: &[Op] = &[
    ("ADC", IMM|ABS|BP|IND_X|IND_Y|IND_Z|BP_X|ABS_X|ABS_Y,
    &[0x69, 0x6D, 0x65, 0x61, 0x71, 0x72, 0x75, 0x7D, 0x79]),
    ("AND", IMM|ABS|BP|IND_X|IND_Y|IND_Z|BP_X|ABS_X|ABS_Y,
    &[0x29, 0x2D, 0x25, 0x21, 0x31, 0x32, 0x35, 0x3D, 0x39]),
];

struct Asm<R> {
    lexer: Lexer<R>,
    pc: u16,
    syms: Vec<(String, i32)>,
    emit: bool,
}

impl<R> Asm<R> {
    fn new(lexer: Lexer<R>) -> Self {
        Self {
            lexer,
            pc: 0,
            syms: Vec::new(),
            emit: false,
        }
    }

    fn write(&mut self, byte: u8) -> io::Result<()> {
        Ok(())
    }
}

fn const_word<R: Read>(asm: &mut Asm<R>) -> io::Result<u16> {
    let expr = const_expr(asm)?;
    if (expr as u32) > (u16::MAX as u32) {
        return Err(asm.lexer.err("expression too large to fit in word"));
    }
    Ok(expr as u16)
}

fn const_byte<R: Read>(asm: &mut Asm<R>) -> io::Result<u8> {
    let expr = const_expr(asm)?;
    if (expr as u32) > (u8::MAX as u32) {
        return Err(asm.lexer.err("expression too large to fit in byte"));
    }
    Ok(expr as u8)
}

fn end_of_line<R: Read>(asm: &mut Asm<R>) -> io::Result<()> {
    let t = asm.lexer.peek()?;
    if t == NEWLINE || t == EOF {
        asm.lexer.eat();
        return Ok(());
    }
    return Err(asm.lexer.err("unexpected garbage"))?;
}

fn const_expr<R: Read>(asm: &mut Asm<R>) -> io::Result<i32> {
    expr(asm)?.ok_or_else(|| asm.lexer.err("expression cannot be resolved"))
}

fn expr<R: Read>(asm: &mut Asm<R>) -> io::Result<Option<i32>> {
    let t = asm.lexer.peek()?;
    asm.lexer.eat();
    match t {
        NUMBER => Ok(Some(asm.lexer.number)),
        STAR => Ok(Some(asm.pc as u32 as i32)),
        _ => Err(asm.lexer.err("expected expression")),
    }
}

fn db<R: Read>(asm: &mut Asm<R>) -> io::Result<()> {
    loop {
        if asm.emit {
            let byte = const_byte(asm)?;
            asm.write(byte)?;
        } else {
            expr(asm)?;
        }
        asm.pc += 1;
        if asm.lexer.peek()? != COMMA {
            break;
        }
        asm.lexer.eat();
    }
    Ok(())
}

fn dw<R: Read>(asm: &mut Asm<R>) -> io::Result<()> {
    loop {
        if asm.emit {
            let [lo, hi] = const_word(asm)?.to_le_bytes();
            asm.write(lo)?;
            asm.write(hi)?;
        } else {
            expr(asm)?;
        }
        asm.pc += 2;
        if asm.lexer.peek()? != COMMA {
            break;
        }
        asm.lexer.eat();
    }
    Ok(())
}

type POp = (&'static str, fn(&mut Asm<File>) -> io::Result<()>);

#[rustfmt::skip]
const POPS: &[POp] = &[
    ("ORG", |asm| { asm.pc = const_word(asm)?; Ok(()) }),
    ("DB", db),
    ("DW", dw),
];

type Token = u16;

const NEWLINE: Token = b'\n' as u16;
const STAR: Token = b'*' as u16;
const COMMA: Token = b',' as u16;
const EQU: Token = b'=' as u16;
const EOF: Token = 0x8000;
const IDENT: Token = 0x8001;
const NUMBER: Token = 0x8002;
const STRING: Token = 0x8003;
const PSEUDO: Token = 0x8004;

struct Lexer<R> {
    inner: Reader<R>,
    string: String,
    number: i32,
    stash: Option<Token>,
    line: usize,
}

impl<R: Read> Lexer<R> {
    fn new(inner: Reader<R>) -> Self {
        Self {
            inner,
            string: String::new(),
            number: 0,
            stash: None,
            line: 1,
        }
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

            // pseudo op
            if c == b'!' {
                self.inner.eat();
                while let Some(c) = self.inner.peek()? {
                    if !c.is_ascii_alphanumeric() {
                        break;
                    }
                    self.string.push(c as char);
                    self.inner.eat();
                }
                self.stash = Some(PSEUDO);
                return Ok(PSEUDO);
            }

            // string
            if c == b'\'' {
                self.inner.eat();
                while let Some(c) = self.inner.peek()? {
                    if c == b'\'' {
                        self.inner.eat();
                        break;
                    }
                    self.string.push(c as char);
                    self.inner.eat();
                }
                self.stash = Some(STRING);
                return Ok(STRING);
            }

            // idents and single chars
            while let Some(c) = self.inner.peek()? {
                if !c.is_ascii_alphanumeric() {
                    break;
                }
                self.string.push(c as char);
                self.inner.eat();
            }
            if self.string.len() > 1 {
                self.stash = Some(IDENT);
                return Ok(IDENT);
            }
            self.stash = Some(c as u16);
            return Ok(c as u16);
        }

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

impl<R: Read> Reader<R> {
    fn new(inner: R) -> Self {
        Self { inner, stash: None }
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

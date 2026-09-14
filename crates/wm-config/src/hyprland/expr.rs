//! Hyprland's layout expressions — the arithmetic a window rule can
//! write in place of a number: `size (monitor_h*4/25) (monitor_h*9/50)`,
//! `move (monitor_w-window_w-40) (monitor_h*0.04)`.
//!
//! Compiled once, when the rule is read, and evaluated on the
//! compositor's thread when a window maps — against the monitor it
//! maps onto, which is the one thing a config reader does not have.
//! So the compiled form is a small tree of pure arithmetic: no
//! allocation past parsing, no I/O, and no way for the text to make
//! evaluation take longer than the tree is deep.
//!
//! # Grammar
//!
//! ```text
//! expr  := term (('+' | '-') term)*
//! term  := unary (('*' | '/') unary)*
//! unary := '-' unary | atom
//! atom  := number | 'monitor_w' | 'monitor_h' | 'window_w' | 'window_h' | '(' expr ')'
//! ```
//!
//! Values are `f64`. Whitespace is not part of the grammar because the
//! rule syntax splits its arguments on it, exactly as Hyprland does.
//!
//! # Bounds
//!
//! The text arrives from a file this desktop does not own. Source
//! longer than [`MAX_SOURCE_BYTES`] or nested deeper than
//! [`MAX_DEPTH`] parentheses is refused at parse time, so the
//! recursion below — parsing and evaluation alike — is bounded by a
//! constant rather than by the file. A result that is not finite
//! (division by zero, overflow) is `None`, and the caller drops the
//! property rather than placing a window at infinity.

/// The longest expression that is read. Omarchy's longest is 30 bytes.
pub(crate) const MAX_SOURCE_BYTES: usize = 128;

/// The deepest parenthesis nesting that is read. Omarchy's deepest is 1.
pub(crate) const MAX_DEPTH: usize = 16;

/// The four names an expression may use, and what each meant to the
/// rule that wrote it: the monitor the window maps on, and the window's
/// own visual size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Var {
    MonitorW,
    MonitorH,
    WindowW,
    WindowH,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

impl Op {
    fn apply(self, a: f64, b: f64) -> f64 {
        match self {
            Op::Add => a + b,
            Op::Sub => a - b,
            Op::Mul => a * b,
            Op::Div => a / b,
        }
    }
}

/// A compiled expression.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
    Num(f64),
    Var(Var),
    Neg(Box<Expr>),
    Bin(Box<Expr>, Op, Box<Expr>),
}

/// The values the four names take at one evaluation, in logical
/// pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Values {
    pub monitor_w: f64,
    pub monitor_h: f64,
    pub window_w: f64,
    pub window_h: f64,
}

impl Expr {
    /// Compiles `source`, or says why it could not — in words meant for
    /// the log line that names the rule.
    pub(crate) fn parse(source: &str) -> Result<Expr, String> {
        if source.len() > MAX_SOURCE_BYTES {
            return Err(format!("longer than {MAX_SOURCE_BYTES} bytes"));
        }
        let mut parser = Parser { bytes: source.as_bytes(), pos: 0, depth: 0 };
        let expr = parser.expr()?;
        if parser.pos != parser.bytes.len() {
            return Err(format!("unexpected {:?} at byte {}", parser.peek_char(), parser.pos));
        }
        Ok(expr)
    }

    /// The expression's value, or `None` when it is not a finite
    /// number — a divide by zero, an overflow — which a caller treats
    /// as "this property says nothing".
    pub(crate) fn eval(&self, values: &Values) -> Option<f64> {
        let v = match self {
            Expr::Num(n) => *n,
            Expr::Var(Var::MonitorW) => values.monitor_w,
            Expr::Var(Var::MonitorH) => values.monitor_h,
            Expr::Var(Var::WindowW) => values.window_w,
            Expr::Var(Var::WindowH) => values.window_h,
            Expr::Neg(e) => -e.eval(values)?,
            Expr::Bin(a, op, b) => op.apply(a.eval(values)?, b.eval(values)?),
        };
        v.is_finite().then_some(v)
    }

    /// The expression's value when it names no variable — a plain
    /// `875` written as an expression — so a constant rule can still be
    /// answered without a monitor.
    pub(crate) fn constant(&self) -> Option<f64> {
        if self.uses_variables() {
            None
        } else {
            self.eval(&Values::default())
        }
    }

    fn uses_variables(&self) -> bool {
        match self {
            Expr::Num(_) => false,
            Expr::Var(_) => true,
            Expr::Neg(e) => e.uses_variables(),
            Expr::Bin(a, _, b) => a.uses_variables() || b.uses_variables(),
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_char(&self) -> String {
        std::str::from_utf8(&self.bytes[self.pos..])
            .ok()
            .and_then(|rest| rest.chars().next())
            .map(|c| c.to_string())
            .unwrap_or_else(|| "end of input".into())
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let mut lhs = self.term()?;
        loop {
            let op = match self.peek() {
                Some(b'+') => Op::Add,
                Some(b'-') => Op::Sub,
                _ => return Ok(lhs),
            };
            self.pos += 1;
            let rhs = self.term()?;
            lhs = Expr::Bin(Box::new(lhs), op, Box::new(rhs));
        }
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(b'*') => Op::Mul,
                Some(b'/') => Op::Div,
                _ => return Ok(lhs),
            };
            self.pos += 1;
            let rhs = self.unary()?;
            lhs = Expr::Bin(Box::new(lhs), op, Box::new(rhs));
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.peek() == Some(b'-') {
            self.pos += 1;
            // A run of minus signs nests like parentheses do, and is
            // bounded the same way: `--------…` is not a deeper
            // expression than the file is allowed to write.
            self.depth += 1;
            if self.depth > MAX_DEPTH {
                return Err(format!("nested deeper than {MAX_DEPTH} levels"));
            }
            let inner = self.unary();
            self.depth -= 1;
            return Ok(Expr::Neg(Box::new(inner?)));
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(b'(') => {
                self.pos += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(format!("nested deeper than {MAX_DEPTH} levels"));
                }
                let inner = self.expr();
                self.depth -= 1;
                let inner = inner?;
                if self.peek() != Some(b')') {
                    return Err(format!("expected ')' at byte {}", self.pos));
                }
                self.pos += 1;
                Ok(inner)
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => self.number(),
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => self.name(),
            _ => Err(format!("expected a number, a name or '(' at byte {}, found {:?}", self.pos, self.peek_char())),
        }
    }

    fn number(&mut self) -> Result<Expr, String> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'.') {
            self.pos += 1;
        }
        // The slice is ASCII digits and dots by construction.
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|e| e.to_string())?;
        match text.parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(Expr::Num(n)),
            _ => Err(format!("{text:?} is not a number")),
        }
    }

    fn name(&mut self) -> Result<Expr, String> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|e| e.to_string())?;
        let var = match text {
            "monitor_w" => Var::MonitorW,
            "monitor_h" => Var::MonitorH,
            "window_w" => Var::WindowW,
            "window_h" => Var::WindowH,
            other => {
                return Err(format!(
                    "{other:?} is not one of monitor_w, monitor_h, window_w or window_h"
                ))
            }
        };
        Ok(Expr::Var(var))
    }
}

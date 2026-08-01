//! Type 2 charstring encoding.
//!
//! [`CharstringBuilder`] turns an outline (absolute, cubic path commands)
//! into a Type 2 charstring program. Coordinates are rounded to integers
//! (like fontTools with the default `roundTolerance` of 0.5) and encoded as
//! relative deltas.
//!
//! Charstrings can be emitted in generalized form (only `rmoveto`,
//! `rlineto`, `rrcurveto`) or specialized via a port of fontTools'
//! `cffLib.specializer.specializeCommands`, producing the same operator
//! choices as fontmake/ufo2ft at CFF optimization level 1.

use kurbo::{BezPath, PathEl, Point};

use crate::OtRound;

/// Type 2 charstring operators (single byte).
mod opcode {
    pub const VMOVETO: u8 = 4;
    pub const RLINETO: u8 = 5;
    pub const HLINETO: u8 = 6;
    pub const VLINETO: u8 = 7;
    pub const RRCURVETO: u8 = 8;
    pub const ENDCHAR: u8 = 14;
    pub const RMOVETO: u8 = 21;
    pub const HMOVETO: u8 = 22;
    pub const RCURVELINE: u8 = 24;
    pub const RLINECURVE: u8 = 25;
    pub const VVCURVETO: u8 = 26;
    pub const HHCURVETO: u8 = 27;
    pub const VHCURVETO: u8 = 30;
    pub const HVCURVETO: u8 = 31;
}

/// The maximum argument stack depth of a Type 2 charstring interpreter.
const MAX_STACK: usize = 48;

/// Direction category of a delta vector, after fontTools' specializer.
///
/// `Zero` is fontTools' made-up `'0'` category (both components zero),
/// resolved to `H` at the end of specialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cat {
    Zero,
    H,
    V,
    R,
}

impl Cat {
    fn negate(self) -> Cat {
        match self {
            Cat::H => Cat::V,
            Cat::V => Cat::H,
            other => other,
        }
    }

    /// fontTools `_mergeCategories`: `Zero` is a wildcard.
    fn merge(self, other: Cat) -> Option<Cat> {
        match (self, other) {
            (Cat::Zero, b) => Some(b),
            (a, Cat::Zero) => Some(a),
            (a, b) if a == b => Some(a),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Op {
    Move(Cat),
    Line(Cat),
    Curve(Cat, Cat),
    /// `rcurveline`: curves followed by one line
    CurveLine,
    /// `rlinecurve`: lines followed by one curve
    LineCurve,
}

type Command = (Op, Vec<i32>);

/// Categorize a delta vector, keeping only the args the category implies.
///
/// Like fontTools, the `Zero` category keeps a single `0` argument.
fn categorize(dx: i32, dy: i32) -> (Cat, Vec<i32>) {
    match (dx, dy) {
        (0, 0) => (Cat::Zero, vec![0]),
        (0, dy) => (Cat::V, vec![dy]),
        (dx, 0) => (Cat::H, vec![dx]),
        (dx, dy) => (Cat::R, vec![dx, dy]),
    }
}

/// Builds a Type 2 charstring from absolute path commands.
///
/// Input coordinates are rounded with [`OtRound`] before deltas are taken,
/// matching fontTools' `T2CharStringPen` with the default tolerance. Only
/// integer output is supported.
#[derive(Debug, Default)]
pub struct CharstringBuilder {
    commands: Vec<Command>,
    /// current (rounded) position
    pos: (i32, i32),
}

impl CharstringBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn delta_to(&mut self, pt: Point) -> (i32, i32) {
        let (x, y): (i32, i32) = (pt.x.ot_round(), pt.y.ot_round());
        let delta = (x - self.pos.0, y - self.pos.1);
        self.pos = (x, y);
        delta
    }

    pub fn move_to(&mut self, pt: Point) {
        let (dx, dy) = self.delta_to(pt);
        self.commands.push((Op::Move(Cat::R), vec![dx, dy]));
    }

    pub fn line_to(&mut self, pt: Point) {
        let (dx, dy) = self.delta_to(pt);
        self.commands.push((Op::Line(Cat::R), vec![dx, dy]));
    }

    pub fn curve_to(&mut self, p1: Point, p2: Point, p3: Point) {
        let (dx1, dy1) = self.delta_to(p1);
        let (dx2, dy2) = self.delta_to(p2);
        let (dx3, dy3) = self.delta_to(p3);
        self.commands.push((
            Op::Curve(Cat::R, Cat::R),
            vec![dx1, dy1, dx2, dy2, dx3, dy3],
        ));
    }

    /// Close the current contour.
    ///
    /// Type 2 contours close implicitly, so this emits nothing and (like
    /// fontTools' `T2CharStringPen`) leaves the current point at the last
    /// point drawn. Callers that don't want an explicit closing line segment
    /// in the charstring must avoid drawing one, as a segment pen would.
    pub fn close_path(&mut self) {}

    /// Append `path`, rounding coordinates to integers.
    ///
    /// Quadratic segments are converted to exact cubic equivalents.
    pub fn append_path(&mut self, path: &BezPath) {
        for el in path.elements() {
            match *el {
                PathEl::MoveTo(p) => self.move_to(p),
                PathEl::LineTo(p) => self.line_to(p),
                PathEl::CurveTo(p1, p2, p3) => self.curve_to(p1, p2, p3),
                PathEl::QuadTo(p1, p2) => {
                    let p0 = Point::new(self.pos.0 as f64, self.pos.1 as f64);
                    let c1 = p0 + (p1 - p0) * (2.0 / 3.0);
                    let c2 = p2 + (p1 - p2) * (2.0 / 3.0);
                    self.curve_to(c1, c2, p2);
                }
                PathEl::ClosePath => self.close_path(),
            }
        }
    }

    /// Serialize the charstring.
    ///
    /// `width` is the width delta to prepend (already relative to
    /// `nominalWidthX`), or `None` if the glyph advance equals
    /// `defaultWidthX`. When `specialize` is false the program uses only
    /// generalized operators (`rmoveto`/`rlineto`/`rrcurveto`), like
    /// fontmake's `--optimize-cff 0`.
    pub fn build(self, width: Option<i32>, specialize: bool) -> Vec<u8> {
        let commands = if specialize {
            specialize_commands(self.commands)
        } else {
            self.commands
        };
        let mut out = Vec::new();
        if let Some(width) = width {
            encode_int(&mut out, width);
        }
        for (op, args) in &commands {
            for arg in args {
                encode_int(&mut out, *arg);
            }
            out.push(operator_byte(op));
        }
        out.push(opcode::ENDCHAR);
        out
    }
}

fn operator_byte(op: &Op) -> u8 {
    match op {
        Op::Move(Cat::R) => opcode::RMOVETO,
        Op::Move(Cat::H) => opcode::HMOVETO,
        Op::Move(Cat::V) => opcode::VMOVETO,
        Op::Line(Cat::R) => opcode::RLINETO,
        Op::Line(Cat::H) => opcode::HLINETO,
        Op::Line(Cat::V) => opcode::VLINETO,
        Op::Curve(Cat::R, Cat::R) => opcode::RRCURVETO,
        Op::Curve(Cat::H, Cat::H) => opcode::HHCURVETO,
        Op::Curve(Cat::V, Cat::V) => opcode::VVCURVETO,
        Op::Curve(Cat::H, Cat::V) => opcode::HVCURVETO,
        Op::Curve(Cat::V, Cat::H) => opcode::VHCURVETO,
        Op::CurveLine => opcode::RCURVELINE,
        Op::LineCurve => opcode::RLINECURVE,
        _ => unreachable!("unresolved pseudo-operator {op:?}"),
    }
}

/// Encode a Type 2 charstring integer operand.
///
/// Unlike DICT integers there is no 5-byte form; values outside the i16
/// range cannot be represented and do not occur in valid outlines.
fn encode_int(out: &mut Vec<u8>, value: i32) {
    match value {
        -107..=107 => out.push((value + 139) as u8),
        108..=1131 => {
            let v = value - 108;
            out.push(((v >> 8) + 247) as u8);
            out.push((v & 0xff) as u8);
        }
        -1131..=-108 => {
            let v = -value - 108;
            out.push(((v >> 8) + 251) as u8);
            out.push((v & 0xff) as u8);
        }
        -32768..=32767 => {
            out.push(28);
            out.extend_from_slice(&(value as i16).to_be_bytes());
        }
        _ => panic!("charstring operand {value} exceeds the Type 2 integer range"),
    }
}

/// Port of fontTools `cffLib.specializer.specializeCommands` (with
/// `generalizeFirst=False, preserveTopology=False, maxstack=48`), operating
/// on generalized integer commands.
fn specialize_commands(mut commands: Vec<Command>) -> Vec<Command> {
    // 1. Combine successive rmoveto operations.
    let mut i = commands.len().saturating_sub(1);
    while i > 0 {
        if matches!(commands[i].0, Op::Move(Cat::R)) && commands[i].0 == commands[i - 1].0 {
            let (_, cur) = commands.remove(i);
            let prev = &mut commands[i - 1].1;
            prev[0] += cur[0];
            prev[1] += cur[1];
        }
        i -= 1;
    }

    // 2. Specialize into h/v/0 variants.
    for (op, args) in commands.iter_mut() {
        match op {
            Op::Move(Cat::R) | Op::Line(Cat::R) => {
                let (cat, new_args) = categorize(args[0], args[1]);
                match op {
                    Op::Move(_) => *op = Op::Move(cat),
                    _ => *op = Op::Line(cat),
                }
                *args = new_args;
            }
            Op::Curve(Cat::R, Cat::R) => {
                let (c1, mut args1) = categorize(args[0], args[1]);
                let (c2, args2) = categorize(args[4], args[5]);
                args1.extend_from_slice(&args[2..4]);
                args1.extend_from_slice(&args2);
                *op = Op::Curve(c1, c2);
                *args = args1;
            }
            _ => (),
        }
    }

    // 3. Merge or delete redundant operations (changes point numbers).
    let mut i = commands.len();
    while i > 0 {
        i -= 1;
        // A 00curveto is demoted to a (specialized) lineto.
        if commands[i].0 == Op::Curve(Cat::Zero, Cat::Zero) {
            debug_assert_eq!(commands[i].1.len(), 4);
            let (cat, args) = categorize(commands[i].1[1], commands[i].1[2]);
            commands[i] = (Op::Line(cat), args);
        }
        // A 0lineto can be deleted.
        if commands[i].0 == Op::Line(Cat::Zero) {
            commands.remove(i);
            continue;
        }
        // Merge adjacent hlineto's and vlineto's (collinear segments).
        if i > 0
            && matches!(commands[i].0, Op::Line(Cat::H) | Op::Line(Cat::V))
            && commands[i].0 == commands[i - 1].0
        {
            let (_, cur) = commands.remove(i);
            commands[i - 1].1[0] += cur[0];
        }
    }

    // 4. Peephole: revert h/v variants between r-variants if that saves a byte.
    if commands.len() > 2 {
        for i in 1..commands.len() - 1 {
            let prv = commands[i - 1].0.clone();
            let nxt = commands[i + 1].0.clone();
            let (op, args) = &commands[i];
            match op {
                Op::Line(cat @ (Cat::Zero | Cat::H | Cat::V))
                    if prv == Op::Line(Cat::R) && nxt == Op::Line(Cat::R) =>
                {
                    debug_assert_eq!(args.len(), 1);
                    let new_args = if *cat == Cat::V {
                        vec![0, args[0]]
                    } else {
                        vec![args[0], 0]
                    };
                    commands[i] = (Op::Line(Cat::R), new_args);
                }
                Op::Curve(c0, c1)
                    if args.len() == 5
                        && prv == Op::Curve(Cat::R, Cat::R)
                        && nxt == Op::Curve(Cat::R, Cat::R) =>
                {
                    debug_assert!((*c0 == Cat::R) ^ (*c1 == Cat::R));
                    let pos = if *c0 == Cat::V {
                        0
                    } else if *c0 != Cat::R {
                        1
                    } else if *c1 == Cat::V {
                        4
                    } else {
                        5
                    };
                    let mut new_args = args.clone();
                    new_args.insert(pos, 0);
                    commands[i] = (Op::Curve(Cat::R, Cat::R), new_args);
                }
                _ => (),
            }
        }
    }

    // 5. Combine adjacent operators, minding the max stack size.
    let mut stack_use = commands.last().map(|(_, args)| args.len()).unwrap_or(0);
    let mut i = commands.len();
    while i > 1 {
        i -= 1;
        let (op1, _) = &commands[i - 1];
        let (op2, args2) = &commands[i];

        let new_op = match (op1, op2) {
            (Op::Line(Cat::R), Op::Line(Cat::R)) => Some(Op::Line(Cat::R)),
            (Op::Curve(Cat::R, Cat::R), Op::Curve(Cat::R, Cat::R)) => {
                Some(Op::Curve(Cat::R, Cat::R))
            }
            (Op::Line(Cat::R), Op::Curve(Cat::R, Cat::R)) if args2.len() == 6 => {
                Some(Op::LineCurve)
            }
            (Op::Curve(Cat::R, Cat::R), Op::Line(Cat::R)) if args2.len() == 2 => {
                Some(Op::CurveLine)
            }
            (Op::Line(Cat::R), Op::LineCurve) => Some(Op::LineCurve),
            (Op::Curve(Cat::R, Cat::R), Op::CurveLine) => Some(Op::CurveLine),
            (Op::Line(Cat::H), Op::Line(Cat::V)) | (Op::Line(Cat::V), Op::Line(Cat::H)) => {
                Some(op1.clone())
            }
            (Op::Curve(d0, d1), Op::Curve(d2, d3)) => {
                let (d0, d1, d2, d3) = (*d0, *d1, *d2, *d3);
                if d1 == Cat::R || d2 == Cat::R || (d0 == Cat::R && d3 == Cat::R) {
                    None
                } else {
                    match d1.merge(d2) {
                        None => None,
                        Some(d) => {
                            if d0 == Cat::R {
                                d.merge(d3).map(|d| Op::Curve(Cat::R, d))
                            } else if d3 == Cat::R {
                                d0.merge(d.negate()).map(|d0| Op::Curve(d0, Cat::R))
                            } else {
                                d0.merge(d3).map(|d0| Op::Curve(d0, d))
                            }
                        }
                    }
                }
            }
            _ => None,
        };

        let args1_stack_use = commands[i - 1].1.len();
        let combined_stack_use = args1_stack_use.max(commands[i - 1].1.len() + stack_use);
        // Keep strictly below the limit so a subroutinizer can insert calls.
        if let Some(new_op) = new_op.filter(|_| combined_stack_use < MAX_STACK) {
            let (_, args2) = commands.remove(i);
            let entry = &mut commands[i - 1];
            entry.0 = new_op;
            entry.1.extend(args2);
            stack_use = combined_stack_use;
        } else {
            stack_use = args1_stack_use;
        }
    }

    // 6. Resolve remaining made-up operators into real operators.
    for (op, args) in commands.iter_mut() {
        match op {
            Op::Move(Cat::Zero) => *op = Op::Move(Cat::H),
            Op::Line(Cat::Zero) => *op = Op::Line(Cat::H),
            Op::Curve(c0, c1)
                if !matches!(
                    (*c0, *c1),
                    (Cat::R, Cat::R)
                        | (Cat::H, Cat::H)
                        | (Cat::V, Cat::V)
                        | (Cat::V, Cat::H)
                        | (Cat::H, Cat::V)
                ) =>
            {
                let l = args.len();
                let mut op0 = *c0;
                let mut op1 = *c1;
                if (op0 == Cat::R) ^ (op1 == Cat::R) {
                    debug_assert!(l % 2 == 1);
                }
                if op0 == Cat::Zero {
                    op0 = Cat::H;
                }
                if op1 == Cat::Zero {
                    op1 = Cat::H;
                }
                if op0 == Cat::R {
                    op0 = op1;
                }
                if op1 == Cat::R {
                    op1 = op0.negate();
                }
                debug_assert!(matches!(op0, Cat::H | Cat::V) && matches!(op1, Cat::H | Cat::V));

                if l % 2 == 1 {
                    if op0 != op1 {
                        // vhcurveto / hvcurveto: maybe swap the last two args
                        if (op0 == Cat::H) ^ (l % 8 == 1) {
                            args.swap(l - 2, l - 1);
                        }
                    } else if op0 == Cat::H {
                        // hhcurveto: dy1 comes first
                        args.swap(0, 1);
                    }
                }
                *op = Op::Curve(op0, op1);
            }
            _ => (),
        }
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_pen(width: Option<i32>, specialize: bool) -> Vec<u8> {
        // moveTo(50,0) lineTo(250,0) lineTo(250,700) lineTo(50,700) closePath
        let mut pen = CharstringBuilder::new();
        pen.move_to(Point::new(50.0, 0.0));
        pen.line_to(Point::new(250.0, 0.0));
        pen.line_to(Point::new(250.0, 700.0));
        pen.line_to(Point::new(50.0, 700.0));
        pen.close_path();
        pen.build(width, specialize)
    }

    #[test]
    fn rect_specialized_matches_fonttools() {
        // fontTools T2CharStringPen(width=100): program
        // [100, 50, 'hmoveto', 200, 700, -200, 'hlineto', 'endchar']
        // bytes: ef bd 16 f7 5c f9 50 fb 5c 06 0e
        assert_eq!(
            rect_pen(Some(100), true),
            [0xef, 0xbd, 0x16, 0xf7, 0x5c, 0xf9, 0x50, 0xfb, 0x5c, 0x06, 0x0e]
        );
    }

    #[test]
    fn curve_specialization_matches_fonttools() {
        // moveTo(0,0) curveTo((10.4,0),(20,9.6),(20,20)) lineTo(0,20):
        // [0, 'hmoveto', 10, 10, 10, 10, 'hvcurveto', -20, 'hlineto', 'endchar']
        let mut pen = CharstringBuilder::new();
        pen.move_to(Point::new(0.0, 0.0));
        pen.curve_to(
            Point::new(10.4, 0.0),
            Point::new(20.0, 9.6),
            Point::new(20.0, 20.0),
        );
        pen.line_to(Point::new(0.0, 20.0));
        pen.close_path();
        let bytes = pen.build(None, true);
        let mut expected = Vec::new();
        for v in [0i32] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::HMOVETO);
        for v in [10i32, 10, 10, 10] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::HVCURVETO);
        encode_int(&mut expected, -20);
        expected.push(opcode::HLINETO);
        expected.push(opcode::ENDCHAR);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn generalized_output() {
        // Same path, unoptimized:
        // [0, 0, 'rmoveto', 10, 0, 10, 10, 0, 10, 'rrcurveto', -20, 0, 'rlineto', 'endchar']
        let mut pen = CharstringBuilder::new();
        pen.move_to(Point::new(0.0, 0.0));
        pen.curve_to(
            Point::new(10.4, 0.0),
            Point::new(20.0, 9.6),
            Point::new(20.0, 20.0),
        );
        pen.line_to(Point::new(0.0, 20.0));
        pen.close_path();
        let bytes = pen.build(None, false);
        let mut expected = Vec::new();
        for v in [0i32, 0] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::RMOVETO);
        for v in [10i32, 0, 10, 10, 0, 10] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::RRCURVETO);
        for v in [-20i32, 0] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::RLINETO);
        expected.push(opcode::ENDCHAR);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn empty_glyph() {
        let pen = CharstringBuilder::new();
        assert_eq!(pen.build(None, true), [opcode::ENDCHAR]);
        let pen = CharstringBuilder::new();
        // width 100 with nominal 0
        assert_eq!(pen.build(Some(100), true), [0xef, opcode::ENDCHAR]);
    }

    #[test]
    fn close_is_implicit() {
        // closePath emits nothing, and the next moveto is relative to the
        // last point drawn, like fontTools' T2CharStringPen
        let mut pen = CharstringBuilder::new();
        pen.move_to(Point::new(10.0, 10.0));
        pen.line_to(Point::new(100.0, 10.0));
        pen.line_to(Point::new(100.0, 100.0));
        pen.close_path();
        pen.move_to(Point::new(0.0, 0.0));
        pen.line_to(Point::new(0.0, 50.0));
        pen.close_path();
        let bytes = pen.build(None, false);
        let mut expected = Vec::new();
        for v in [10i32, 10] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::RMOVETO);
        for v in [90i32, 0] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::RLINETO);
        for v in [0i32, 90] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::RLINETO);
        // moveto is relative to (100, 100), the last point drawn
        for v in [-100i32, -100] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::RMOVETO);
        for v in [0i32, 50] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::RLINETO);
        expected.push(opcode::ENDCHAR);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn alternating_hv_lines_merge() {
        // The plus-sign shape from fontc's Static-Regular fixture: a long
        // alternating h/v run must merge into a single hlineto, like
        // fontmake produces:
        //   57 242 111 rmoveto 72 206 199 71 -199 207 -72 -207 -199 -71 199 hlineto
        let pts = [
            (242, 111),
            (314, 111),
            (314, 317),
            (513, 317),
            (513, 388),
            (314, 388),
            (314, 595),
            (242, 595),
            (242, 388),
            (43, 388),
            (43, 317),
            (242, 317),
        ];
        let mut pen = CharstringBuilder::new();
        pen.move_to(Point::new(pts[0].0 as f64, pts[0].1 as f64));
        for (x, y) in &pts[1..] {
            pen.line_to(Point::new(*x as f64, *y as f64));
        }
        pen.close_path();
        let bytes = pen.build(Some(57), true);
        let mut expected = Vec::new();
        for v in [57i32, 242, 111] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::RMOVETO);
        for v in [72i32, 206, 199, 71, -199, 207, -72, -207, -199, -71, 199] {
            encode_int(&mut expected, v);
        }
        expected.push(opcode::HLINETO);
        expected.push(opcode::ENDCHAR);
        assert_eq!(bytes, expected);
    }
}

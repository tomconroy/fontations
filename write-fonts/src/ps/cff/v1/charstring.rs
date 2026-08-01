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

use kurbo::{BezPath, CubicBez, ParamCurveExtrema, PathEl, Point, Rect, Vec2};

use super::CffBuildError;
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

/// A compiled charstring and the bounds of the outline it draws.
#[derive(Clone, Debug, PartialEq)]
pub struct Charstring {
    /// The charstring program.
    pub bytes: Vec<u8>,
    /// Exact bounds of the outline *as this program draws it*, or `None` if
    /// it draws nothing.
    ///
    /// ufo2ft measures the compiled charstring, not the source outline, and
    /// specialization can drop geometry: a stray point merges into the
    /// preceding moveto, a backtracking segment merges into a collinear
    /// neighbour. Bounds taken from the source path would enclose points the
    /// font does not contain.
    pub bounds: Option<Rect>,
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
    /// current position before rounding, as fontTools' `BasePen` keeps it
    exact_pos: Point,
}

impl CharstringBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn delta_to(&mut self, pt: Point) -> (i32, i32) {
        let (x, y): (i32, i32) = (pt.x.ot_round(), pt.y.ot_round());
        let delta = (x - self.pos.0, y - self.pos.1);
        self.pos = (x, y);
        self.exact_pos = pt;
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
                    // fontTools' BasePen raises the quadratic from the
                    // *unrounded* current point and rounds the cubic once, so
                    // rounding the input first would round twice
                    let p0 = self.exact_pos;
                    let c1 = p0 + (p1 - p0) * (2.0 / 3.0);
                    let c2 = p2 + (p1 - p2) * (2.0 / 3.0);
                    self.curve_to(c1, c2, p2);
                }
                PathEl::ClosePath => self.close_path(),
            }
        }
    }

    /// Serialize the charstring, and measure what it draws.
    ///
    /// `width` is the width delta to prepend (already relative to
    /// `nominalWidthX`), or `None` if the glyph advance equals
    /// `defaultWidthX`. When `specialize` is false the program uses only
    /// generalized operators (`rmoveto`/`rlineto`/`rrcurveto`), like
    /// fontmake's `--optimize-cff 0`.
    pub fn build(self, width: Option<i32>, specialize: bool) -> Result<Charstring, CffBuildError> {
        // passes 1-3 add, merge and delete segments, so they run before the
        // outline is measured; passes 4-6 only choose an encoding for it
        let commands = if specialize {
            specialize_geometry(self.commands)
        } else {
            self.commands
        };
        let bounds = command_bounds(&commands);
        let commands = if specialize {
            specialize_encoding(commands)
        } else {
            commands
        };

        let mut bytes = Vec::new();
        if let Some(width) = width {
            encode_int(&mut bytes, width)?;
        }
        for (op, args) in &commands {
            for arg in args {
                encode_int(&mut bytes, *arg)?;
            }
            bytes.push(operator_byte(op));
        }
        bytes.push(opcode::ENDCHAR);
        Ok(Charstring { bytes, bounds })
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
/// Unlike DICT integers there is no 5-byte integer form; a value outside the
/// i16 range cannot be represented. (fontTools emits the 16.16 fixed form for
/// these, which silently mangles the value, so we report an error instead.)
pub(super) fn encode_int(out: &mut Vec<u8>, value: i32) -> Result<(), CffBuildError> {
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
        _ => return Err(CffBuildError::OperandOutOfRange { value }),
    }
    Ok(())
}

/// Bounds of the outline a command list draws, like fontTools' `BoundsPen`:
/// curve-exact, and a moveto contributes its point even if nothing follows.
///
/// Expects commands in the form specialization passes 1-3 leave them, where
/// every command is still a single segment.
fn command_bounds(commands: &[Command]) -> Option<Rect> {
    /// The delta a category-reduced argument list encodes, and how many
    /// arguments it consumed.
    fn delta(cat: Cat, args: &[i32]) -> (Vec2, usize) {
        match cat {
            Cat::Zero => (Vec2::ZERO, 1),
            Cat::H => (Vec2::new(args[0] as f64, 0.0), 1),
            Cat::V => (Vec2::new(0.0, args[0] as f64), 1),
            Cat::R => (Vec2::new(args[0] as f64, args[1] as f64), 2),
        }
    }

    let mut bounds: Option<Rect> = None;
    let mut pos = Point::ZERO;
    for (op, args) in commands {
        match op {
            Op::Move(cat) | Op::Line(cat) => {
                pos += delta(*cat, args).0;
                bounds = Some(match bounds {
                    Some(bounds) => bounds.union_pt(pos),
                    None => Rect::from_points(pos, pos),
                });
            }
            Op::Curve(c0, c1) => {
                let (d1, used) = delta(*c0, args);
                let p1 = pos + d1;
                let p2 = p1 + Vec2::new(args[used] as f64, args[used + 1] as f64);
                let p3 = p2 + delta(*c1, &args[used + 2..]).0;
                let curve = CubicBez::new(pos, p1, p2, p3).bounding_box();
                bounds = Some(match bounds {
                    Some(bounds) => bounds.union(curve),
                    None => curve,
                });
                pos = p3;
            }
            Op::CurveLine | Op::LineCurve => {
                unreachable!("outlines are measured before operators are combined")
            }
        }
    }
    bounds
}

/// Outcome of testing whether two adjacent operators can be combined.
///
/// fontTools distinguishes two kinds of failure: falling off the end of the
/// merge logic with `new_op = None` (which still records the left operand's
/// stack use), and `continue`, which leaves `stackUse` untouched. The
/// difference is observable: a stale, larger `stackUse` refuses later merges.
enum Merge {
    Into(Op),
    No,
    Skip,
}

/// Passes 1-3 of fontTools `cffLib.specializer.specializeCommands` (with
/// `generalizeFirst=False, preserveTopology=False, maxstack=48`), operating
/// on generalized integer commands.
///
/// These are the passes that change which points the outline contains.
fn specialize_geometry(mut commands: Vec<Command>) -> Vec<Command> {
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

    commands
}

/// Passes 4-6 of fontTools' `specializeCommands`: they pick operators and
/// argument layouts for an outline that is already final.
fn specialize_encoding(mut commands: Vec<Command>) -> Vec<Command> {
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

        let outcome = match (op1, op2) {
            (Op::Line(Cat::R), Op::Line(Cat::R)) => Merge::Into(Op::Line(Cat::R)),
            (Op::Curve(Cat::R, Cat::R), Op::Curve(Cat::R, Cat::R)) => {
                Merge::Into(Op::Curve(Cat::R, Cat::R))
            }
            (Op::Line(Cat::R), Op::Curve(Cat::R, Cat::R)) if args2.len() == 6 => {
                Merge::Into(Op::LineCurve)
            }
            (Op::Curve(Cat::R, Cat::R), Op::Line(Cat::R)) if args2.len() == 2 => {
                Merge::Into(Op::CurveLine)
            }
            (Op::Line(Cat::R), Op::LineCurve) => Merge::Into(Op::LineCurve),
            (Op::Curve(Cat::R, Cat::R), Op::CurveLine) => Merge::Into(Op::CurveLine),
            (Op::Line(Cat::H), Op::Line(Cat::V)) | (Op::Line(Cat::V), Op::Line(Cat::H)) => {
                Merge::Into(op1.clone())
            }
            (Op::Curve(d0, d1), Op::Curve(d2, d3)) => {
                let (d0, d1, d2, d3) = (*d0, *d1, *d2, *d3);
                // Every rejection in this branch is a `continue` in fontTools,
                // which skips the stack-use bookkeeping below.
                if d1 == Cat::R || d2 == Cat::R || (d0 == Cat::R && d3 == Cat::R) {
                    Merge::Skip
                } else {
                    match d1.merge(d2) {
                        None => Merge::Skip,
                        Some(d) => {
                            let merged = if d0 == Cat::R {
                                d.merge(d3).map(|d| Op::Curve(Cat::R, d))
                            } else if d3 == Cat::R {
                                d0.merge(d.negate()).map(|d0| Op::Curve(d0, Cat::R))
                            } else {
                                d0.merge(d3).map(|d0| Op::Curve(d0, d))
                            };
                            merged.map_or(Merge::Skip, Merge::Into)
                        }
                    }
                }
            }
            _ => Merge::No,
        };
        let new_op = match outcome {
            // fontTools reaches the next iteration without touching stackUse,
            // so the stale value keeps blocking merges further left.
            Merge::Skip => continue,
            Merge::No => None,
            Merge::Into(op) => Some(op),
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
        pen.build(width, specialize).unwrap().bytes
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
        let bytes = pen.build(None, true).unwrap().bytes;
        let mut expected = Vec::new();
        for v in [0i32] {
            encode_int(&mut expected, v).unwrap();
        }
        expected.push(opcode::HMOVETO);
        for v in [10i32, 10, 10, 10] {
            encode_int(&mut expected, v).unwrap();
        }
        expected.push(opcode::HVCURVETO);
        encode_int(&mut expected, -20).unwrap();
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
        let bytes = pen.build(None, false).unwrap().bytes;
        let mut expected = Vec::new();
        for v in [0i32, 0] {
            encode_int(&mut expected, v).unwrap();
        }
        expected.push(opcode::RMOVETO);
        for v in [10i32, 0, 10, 10, 0, 10] {
            encode_int(&mut expected, v).unwrap();
        }
        expected.push(opcode::RRCURVETO);
        for v in [-20i32, 0] {
            encode_int(&mut expected, v).unwrap();
        }
        expected.push(opcode::RLINETO);
        expected.push(opcode::ENDCHAR);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn empty_glyph() {
        let pen = CharstringBuilder::new();
        assert_eq!(pen.build(None, true).unwrap().bytes, [opcode::ENDCHAR]);
        let pen = CharstringBuilder::new();
        // width 100 with nominal 0
        assert_eq!(
            pen.build(Some(100), true).unwrap().bytes,
            [0xef, opcode::ENDCHAR]
        );
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
        let bytes = pen.build(None, false).unwrap().bytes;
        let mut expected = Vec::new();
        for v in [10i32, 10] {
            encode_int(&mut expected, v).unwrap();
        }
        expected.push(opcode::RMOVETO);
        for v in [90i32, 0] {
            encode_int(&mut expected, v).unwrap();
        }
        expected.push(opcode::RLINETO);
        for v in [0i32, 90] {
            encode_int(&mut expected, v).unwrap();
        }
        expected.push(opcode::RLINETO);
        // moveto is relative to (100, 100), the last point drawn
        for v in [-100i32, -100] {
            encode_int(&mut expected, v).unwrap();
        }
        expected.push(opcode::RMOVETO);
        for v in [0i32, 50] {
            encode_int(&mut expected, v).unwrap();
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
        let bytes = pen.build(Some(57), true).unwrap().bytes;
        let mut expected = Vec::new();
        for v in [57i32, 242, 111] {
            encode_int(&mut expected, v).unwrap();
        }
        expected.push(opcode::RMOVETO);
        for v in [72i32, 206, 199, 71, -199, 207, -72, -207, -199, -71, 199] {
            encode_int(&mut expected, v).unwrap();
        }
        expected.push(opcode::HLINETO);
        expected.push(opcode::ENDCHAR);
        assert_eq!(bytes, expected);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// When two curves can't merge, fontTools `continue`s, so `stackUse` keeps
    /// the value from further right instead of dropping to the left operand's.
    /// The stale (larger) value then refuses merges that a reset one would
    /// allow, which is visible here as two vvcurveto groups where a naive port
    /// emits one.
    #[test]
    fn stale_stack_use_splits_vvcurveto() {
        let mut pen = CharstringBuilder::new();
        pen.move_to(Point::new(0.0, 0.0));
        pen.curve_to(
            Point::new(10.0, 20.0),
            Point::new(40.0, 60.0),
            Point::new(40.0, 110.0),
        );
        pen.curve_to(
            Point::new(40.0, 120.0),
            Point::new(60.0, 150.0),
            Point::new(60.0, 190.0),
        );
        let (mut x, mut y) = (60.0, 190.0);
        for _ in 0..11 {
            pen.curve_to(
                Point::new(x + 30.0, y),
                Point::new(x + 60.0, y + 10.0),
                Point::new(x + 90.0, y + 10.0),
            );
            x += 90.0;
            y += 10.0;
        }
        pen.close_path();

        // fontTools T2CharStringPen(None, None).getCharString(optimize=True):
        // [0 hmoveto | 10 20 30 40 50 vvcurveto | 10 20 30 40 vvcurveto
        //  | 30 30 10 30 (x11) hhcurveto | endchar]
        assert_eq!(
            hex(&pen.build(None, true).unwrap().bytes),
            "8b16959fa9b3bd1a959fa9b31aa9a995a9a9a995a9a9a995a9a9a995a9a9a995\
             a9a9a995a9a9a995a9a9a995a9a9a995a9a9a995a9a9a995a91b0e"
        );
    }

    /// Bounds describe the program, not the source outline: specialization
    /// merged the stray point's moveto into the next one, so fontTools'
    /// `calcBounds` never sees (100, 100).
    #[test]
    fn bounds_exclude_a_merged_away_stray_point() {
        let mut pen = CharstringBuilder::new();
        pen.move_to(Point::new(100.0, 100.0));
        pen.close_path();
        pen.move_to(Point::new(0.0, 0.0));
        pen.line_to(Point::new(50.0, 0.0));
        pen.line_to(Point::new(50.0, 50.0));
        pen.close_path();
        let cs = pen.build(None, true).unwrap();
        assert_eq!(hex(&cs.bytes), "8b16bdbd060e");
        assert_eq!(cs.bounds, Some(Rect::new(0.0, 0.0, 50.0, 50.0)));
    }

    /// Two collinear hlinetos merge into their sum, taking the turning point
    /// (and with it the outline's x_max) out of the program.
    #[test]
    fn bounds_exclude_a_merged_away_backtrack() {
        let mut pen = CharstringBuilder::new();
        pen.move_to(Point::new(0.0, 0.0));
        pen.line_to(Point::new(100.0, 0.0));
        pen.line_to(Point::new(-20.0, 0.0));
        pen.close_path();
        let cs = pen.build(None, true).unwrap();
        assert_eq!(hex(&cs.bytes), "8b1677060e");
        assert_eq!(cs.bounds, Some(Rect::new(-20.0, 0.0, 0.0, 0.0)));
    }

    /// Curve bounds are exact, not control-point bounds: fontTools reports
    /// y_max 75, not 100.
    #[test]
    fn bounds_use_curve_extrema() {
        let mut pen = CharstringBuilder::new();
        pen.move_to(Point::new(0.0, 0.0));
        pen.curve_to(
            Point::new(10.0, 100.0),
            Point::new(90.0, 100.0),
            Point::new(100.0, 0.0),
        );
        pen.close_path();
        let bounds = pen.build(None, true).unwrap().bounds.unwrap();
        assert_eq!((bounds.x0, bounds.y0, bounds.x1), (0.0, 0.0, 100.0));
        assert!((bounds.y1 - 75.0).abs() < 1e-9, "y1 = {}", bounds.y1);
    }

    /// A contour that is only a moveto still contributes its point.
    #[test]
    fn bounds_of_a_lone_point() {
        let mut pen = CharstringBuilder::new();
        pen.move_to(Point::new(30.0, -40.0));
        pen.close_path();
        let cs = pen.build(None, true).unwrap();
        assert_eq!(cs.bounds, Some(Rect::new(30.0, -40.0, 30.0, -40.0)));
        assert_eq!(
            CharstringBuilder::new().build(None, true).unwrap().bounds,
            None
        );
    }

    /// The 2/3 rule applies to the unrounded points; rounding the quadratic
    /// first and converting from the rounded current point would put the first
    /// control point one unit off (-19 instead of -20 here).
    #[test]
    fn quadratic_conversion_rounds_once() {
        let mut path = BezPath::new();
        path.move_to(Point::new(-176.8, 3.0));
        path.quad_to(Point::new(-185.0, -26.5), Point::new(-172.1, -163.7));
        path.close_path();
        let mut pen = CharstringBuilder::new();
        pen.append_path(&path);
        // fontTools: [-177, 3, 'rmoveto', -5, -20, 1, -55, 9, -92, 'rrcurveto']
        assert_eq!(
            hex(&pen.build(None, true).unwrap().bytes),
            "fb458e1586778c54942f080e"
        );
    }

    /// Type 2 has no integer operand wide enough for this; fontTools would
    /// silently write a 16.16 fixed number.
    #[test]
    fn operand_out_of_range_is_an_error() {
        let mut pen = CharstringBuilder::new();
        pen.move_to(Point::new(0.0, 0.0));
        pen.line_to(Point::new(40000.0, 0.0));
        assert_eq!(
            pen.build(None, true),
            Err(CffBuildError::OperandOutOfRange { value: 40000 })
        );
    }
}

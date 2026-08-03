//! CFF2 charstrings: merging per-master outlines into one blended program.
//!
//! Where a CFF1 charstring is drawn from one outline, a CFF2 charstring is
//! drawn from every master's outline at once: arguments that agree across
//! masters stay plain numbers, and arguments that differ become the default
//! master's value plus one delta per variation region, spelled out with the
//! `blend` operator.
//!
//! This is a port of `fontTools.varLib.cff`'s `CFF2CharStringMergePen`
//! (`varLib/cff.py:465-631`) and the last pass of `cffLib`'s specializer,
//! `_convertToBlendCmds` (`cffLib/specializer.py:448-503`), so the programs
//! match `fontmake -o variable-cff2 --optimize-cff 1` given the same
//! outlines and the same variation model.

use kurbo::Rect;

use crate::ps::cff::{
    charstring::{
        command_bounds, encode_fixed, encode_int, opcode, operator_byte, specialize_encoding,
        specialize_geometry, Arg, CharstringBuilder, Command, Op, MAX_STACK_CFF2,
    },
    CffBuildError,
};

/// Builds one glyph's CFF2 charstring from its outline in every master.
///
/// Masters are in variation-model order, the default master first, and must
/// all draw the same sequence of commands: fontTools raises
/// `VarLibCFFPointTypeMergeError` when they do not, and so do we. A master in
/// which the glyph does not participate is not passed in at all — the caller
/// builds a sub-model over the participating masters, exactly as
/// `merge_charstrings` does (`varLib/cff.py:315-318`).
#[derive(Debug)]
pub struct Cff2CharstringBuilder {
    masters: Vec<CharstringBuilder>,
}

impl Cff2CharstringBuilder {
    /// A builder for `master_count` masters, the first of which is the
    /// default master.
    pub fn new(master_count: usize) -> Self {
        Cff2CharstringBuilder {
            masters: (0..master_count)
                .map(|_| CharstringBuilder::new())
                .collect(),
        }
    }

    /// The pen for one master's outline, by index in variation-model order.
    pub fn master(&mut self, index: usize) -> &mut CharstringBuilder {
        &mut self.masters[index]
    }

    /// Merge the masters and compile the program.
    ///
    /// `deltas` is the variation model's delta computation: given one value
    /// per master, in the order the masters were added, it returns one delta
    /// per master. The first of those (the default master's own value) is
    /// dropped, as `reorder_blend_args` does (`varLib/cff.py:601-612`).
    /// Deltas are used exactly as returned — varLib rounds them only within a
    /// 0.01 tolerance (`varLib/cff.py:482`), so whatever fractions survive
    /// are written as 16.16 fixed operands.
    ///
    /// The returned program carries no `vsindex`; see
    /// [`Cff2Charstring::with_vsindex`].
    pub fn build(
        self,
        deltas: impl Fn(&[f64]) -> Vec<f64>,
    ) -> Result<Cff2Charstring, CffBuildError> {
        let master_count = self.masters.len();
        if master_count == 0 {
            return Err(CffBuildError::NoMasters);
        }
        let masters: Vec<Vec<Command>> = self
            .masters
            .into_iter()
            .map(CharstringBuilder::into_commands)
            .collect();
        let commands = merge_masters(&masters, &deltas)?;

        let marking = commands.iter().any(|(op, _)| matches!(op, Op::Move(_)));
        let commands = specialize_geometry(commands);
        let bounds = command_bounds(&commands);
        let commands = specialize_encoding(commands, MAX_STACK_CFF2);

        // no width prefix and no endchar: a CFF2 charstring has neither
        // (`CFFToCFF2.py:92-123`, `psCharStrings.py:1197-1200`)
        let mut bytes = Vec::new();
        let mut blends = false;
        for (op, args) in &commands {
            blends |= write_args(&mut bytes, args)?;
            bytes.push(operator_byte(op));
        }
        Ok(Cff2Charstring {
            bytes,
            bounds,
            blends,
            marking,
        })
    }
}

/// A compiled CFF2 charstring.
#[derive(Clone, Debug, PartialEq)]
pub struct Cff2Charstring {
    /// The charstring program.
    pub bytes: Vec<u8>,
    /// Exact bounds of the outline the program draws in the *default*
    /// master, or `None` if it draws nothing.
    pub bounds: Option<Rect>,
    /// Whether the program contains a `blend`.
    ///
    /// varLib only assigns a variation store index to a glyph that blends
    /// (`varLib/cff.py:349-353`), so this is what a caller allocating
    /// vsindexes in glyph order has to look at.
    pub blends: bool,
    /// Whether the glyph draws anything: fontTools' `seen_moveto`.
    pub marking: bool,
}

impl Cff2Charstring {
    /// Prefix the program with `vsindex`, selecting the variation store
    /// subtable this glyph's deltas belong to.
    ///
    /// Nothing is written for store index 0, or for a glyph that does not
    /// blend or does not draw: varLib skips those glyphs before it ever
    /// allocates an index (`varLib/cff.py:349-367`).
    pub fn with_vsindex(mut self, vsindex: u16) -> Result<Self, CffBuildError> {
        if vsindex == 0 || !self.blends || !self.marking {
            return Ok(self);
        }
        let mut program = Vec::with_capacity(self.bytes.len() + 4);
        encode_int(&mut program, vsindex as i32)?;
        program.push(opcode::VSINDEX);
        program.extend_from_slice(&self.bytes);
        self.bytes = program;
        Ok(self)
    }
}

/// `reorder_blend_args` (`varLib/cff.py:559-614`): one argument list per
/// master becomes one list of arguments that each blend across masters.
fn merge_masters(
    masters: &[Vec<Command>],
    deltas: &impl Fn(&[f64]) -> Vec<f64>,
) -> Result<Vec<Command>, CffBuildError> {
    let default = &masters[0];
    for (index, master) in masters.iter().enumerate().skip(1) {
        let common = master.len().min(default.len());
        let mismatch = (0..common)
            .find(|&i| master[i].0 != default[i].0 || master[i].1.len() != default[i].1.len())
            .or_else(|| (master.len() != default.len()).then_some(common));
        if let Some(command) = mismatch {
            return Err(CffBuildError::IncompatibleMasters {
                master: index,
                command,
            });
        }
    }

    let mut merged = Vec::with_capacity(default.len());
    let mut values = Vec::with_capacity(masters.len());
    for (index, (op, args)) in default.iter().enumerate() {
        let mut new_args = Vec::with_capacity(args.len());
        for arg in 0..args.len() {
            values.clear();
            values.extend(
                masters
                    .iter()
                    .map(|master| master[index].1[arg].default_value()),
            );
            if values.iter().all(|value| *value == values[0]) {
                new_args.push(Arg::Scalar(values[0]));
                continue;
            }
            let as_f64: Vec<f64> = values.iter().map(|value| *value as f64).collect();
            let computed = deltas(&as_f64);
            if computed.len() != masters.len() {
                return Err(CffBuildError::WrongDeltaCount {
                    expected: masters.len(),
                    got: computed.len(),
                });
            }
            new_args.push(Arg::Blend {
                default: values[0],
                deltas: computed[1..].to_vec(),
            });
        }
        merged.push((op.clone(), new_args));
    }
    Ok(merged)
}

/// `_convertToBlendCmds` (`cffLib/specializer.py:448-503`) fused with the
/// operand encoding: a run of consecutive blended arguments is written as one
/// `blend`, with every default value first, then every argument's deltas, then
/// the number of arguments in the run.
///
/// Returns whether anything was blended.
///
/// The run is bounded by the CFF2 stack limit, and *only* by it — the
/// specializer deliberately uses 513 here rather than the caller's `maxstack`,
/// because that limit would otherwise cap how many masters a font can have
/// (`specializer.py:474-482`).
fn write_args(out: &mut Vec<u8>, args: &[Arg]) -> Result<bool, CffBuildError> {
    let mut blended = false;
    let mut stack_use = 0usize;
    let mut i = 0;
    while i < args.len() {
        let Arg::Blend { deltas, .. } = &args[i] else {
            encode_int(out, args[i].default_value())?;
            stack_use += 1;
            i += 1;
            continue;
        };
        // one operand per master, plus the count of blended arguments
        let sources = deltas.len() + 1;
        let start = i;
        let stack_use_before = stack_use;
        stack_use += 1 + sources;
        i += 1;
        while i < args.len()
            && matches!(args[i], Arg::Blend { .. })
            && stack_use + sources < MAX_STACK_CFF2
        {
            stack_use += sources;
            i += 1;
        }

        let run = &args[start..i];
        for arg in run {
            encode_int(out, arg.default_value())?;
        }
        for arg in run {
            for delta in arg.deltas() {
                encode_fixed(out, *delta)?;
            }
        }
        encode_int(out, run.len() as i32)?;
        out.push(opcode::BLEND);
        blended = true;
        // the blend leaves one value per blended argument behind
        stack_use = stack_use_before + run.len();
    }
    Ok(blended)
}

#[cfg(test)]
mod tests {
    use kurbo::Point;

    use super::*;

    /// The variation model of the `Vf3` fixture: three masters on one axis at
    /// normalized 0 (default), 0.4 and 1.0, whose regions are `(0, 0.4, 1)`
    /// and `(0.4, 1, 1)`. The second region's scalar at the intermediate
    /// master is 0, so both deltas are simply differences from the default —
    /// checked against `fontTools.varLib.models.VariationModel.getDeltas`.
    fn vf3_deltas(values: &[f64]) -> Vec<f64> {
        vec![values[0], values[1] - values[0], values[2] - values[0]]
    }

    /// Two masters: one region, one delta.
    fn two_master_deltas(values: &[f64]) -> Vec<f64> {
        vec![values[0], values[1] - values[0]]
    }

    /// `box(x, y, w, h)` from the fixture generator, drawn with a pen.
    fn draw_box(pen: &mut CharstringBuilder, x: f64, y: f64, w: f64, h: f64) {
        pen.move_to(Point::new(x, y));
        pen.line_to(Point::new(x + w, y));
        pen.line_to(Point::new(x + w, y + h));
        pen.line_to(Point::new(x, y + h));
        pen.close_path();
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// `Full` in the Vf3 fixture: a box that varies in width and height in
    /// all three masters, so all three of its varying arguments blend
    /// together in one `blend`.
    ///
    /// fontmake writes `1 vsindex 0 hmoveto 100 301 -100 120 300 40 100 -120
    /// -300 3 blend hlineto`.
    #[test]
    fn full_matches_fontmake() {
        let mut builder = Cff2CharstringBuilder::new(3);
        for (index, t) in [0.0, 40.0, 100.0].into_iter().enumerate() {
            draw_box(builder.master(index), 0.0, 0.0, 100.0 + 3.0 * t, 301.0 + t);
        }
        let cs = builder.build(vf3_deltas).unwrap();
        assert!(cs.blends && cs.marking);
        assert_eq!(hex(&cs.bytes), "8b16eff7c127f70cf7c0b3effb0cfbc08e1006");
        assert_eq!(
            hex(&cs.with_vsindex(1).unwrap().bytes),
            "8c0f8b16eff7c127f70cf7c0b3effb0cfbc08e1006"
        );
    }

    /// `Sparse`: present in two masters only, and its two varying arguments
    /// are separated by a static one, which splits them into two blends.
    ///
    /// fontmake writes `0 hmoveto 100 100 1 blend 300 -100 -100 1 blend
    /// hlineto` — and no vsindex, because this glyph is the one that takes
    /// store index 0.
    #[test]
    fn sparse_matches_fontmake() {
        let mut builder = Cff2CharstringBuilder::new(2);
        for (index, t) in [0.0, 100.0].into_iter().enumerate() {
            draw_box(builder.master(index), 0.0, 0.0, 100.0 + t, 300.0);
        }
        let cs = builder.build(two_master_deltas).unwrap();
        assert_eq!(hex(&cs.bytes), "8b16efef8c10f7c027278c1006");
        // store index 0 is never written
        assert_eq!(cs.clone().with_vsindex(0).unwrap(), cs);
    }

    /// `Curvy`: a curve whose `dy2` is the same in every master, so the six
    /// arguments of the `rrcurveto` become a run of three blends, a plain
    /// `0`, and a run of two blends.
    ///
    /// fontmake writes `1 vsindex 20 100 rmoveto 100 250 100 13 33 80 200 13
    /// 33 3 blend 0 100 -250 14 34 -80 -200 2 blend rrcurveto -50 vlineto`.
    #[test]
    fn curvy_matches_fontmake() {
        let mut builder = Cff2CharstringBuilder::new(3);
        for (index, t) in [0.0, 40.0, 100.0].into_iter().enumerate() {
            let (x, y, w, h) = (20.0, 100.0, 300.0 + t, 250.0 + 2.0 * t);
            let pen = builder.master(index);
            pen.move_to(Point::new(x, y));
            // the fixture draws the controls at `x + w // 3` and
            // `x + 2 * w // 3`, with Python's integer division
            let third = (w as i64 / 3) as f64;
            let two_thirds = (2 * w as i64 / 3) as f64;
            pen.curve_to(
                Point::new(x + third, y + h),
                Point::new(x + two_thirds, y + h),
                Point::new(x + w, y),
            );
            pen.line_to(Point::new(x + w, y - 50.0));
            pen.close_path();
        }
        let cs = builder.build(vf3_deltas).unwrap().with_vsindex(1).unwrap();
        assert_eq!(
            hex(&cs.bytes),
            "8c0f9fef15eff78eef98acdbf75c98ac8e108beffb8e99ad3bfb5c8d10085907"
        );
    }

    /// `Static` is identical in every master: no blend, and so no vsindex
    /// however high a store index the caller offers.
    #[test]
    fn static_glyph_does_not_blend() {
        let mut builder = Cff2CharstringBuilder::new(3);
        for index in 0..3 {
            draw_box(builder.master(index), 10.0, 10.0, 200.0, 400.0);
        }
        let cs = builder.build(vf3_deltas).unwrap();
        assert!(!cs.blends);
        assert_eq!(hex(&cs.bytes), "959515f75cf824fb5c06");
        assert_eq!(cs.clone().with_vsindex(2).unwrap().bytes, cs.bytes);
    }

    /// An empty glyph compiles to nothing at all — no `endchar`, unlike CFF1.
    #[test]
    fn empty_glyph_is_empty() {
        let cs = Cff2CharstringBuilder::new(3).build(vf3_deltas).unwrap();
        assert!(cs.bytes.is_empty());
        assert!(!cs.marking);
        assert_eq!(cs.bounds, None);
    }

    /// Bounds describe the default master's outline, which is what the
    /// default instance's metrics are measured from.
    #[test]
    fn bounds_are_the_default_masters() {
        let mut builder = Cff2CharstringBuilder::new(2);
        draw_box(builder.master(0), 0.0, 0.0, 100.0, 300.0);
        draw_box(builder.master(1), 0.0, 0.0, 200.0, 300.0);
        let cs = builder.build(two_master_deltas).unwrap();
        assert_eq!(cs.bounds, Some(Rect::new(0.0, 0.0, 100.0, 300.0)));
    }

    #[test]
    fn incompatible_masters_are_an_error() {
        let mut builder = Cff2CharstringBuilder::new(2);
        draw_box(builder.master(0), 0.0, 0.0, 100.0, 300.0);
        builder.master(1).move_to(Point::new(0.0, 0.0));
        builder.master(1).curve_to(
            Point::new(10.0, 10.0),
            Point::new(20.0, 20.0),
            Point::new(30.0, 0.0),
        );
        assert_eq!(
            builder.build(two_master_deltas),
            Err(CffBuildError::IncompatibleMasters {
                master: 1,
                command: 1
            })
        );
    }

    /// A fractional delta survives as a 16.16 fixed operand: varLib rounds a
    /// delta only when it is already within 0.01 of an integer.
    #[test]
    fn fractional_deltas_are_fixed_operands() {
        let mut builder = Cff2CharstringBuilder::new(2);
        draw_box(builder.master(0), 0.0, 0.0, 100.0, 300.0);
        draw_box(builder.master(1), 0.0, 0.0, 200.0, 300.0);
        let cs = builder
            .build(|values| vec![values[0], (values[1] - values[0]) + 0.56])
            .unwrap();
        // 100 100.56 1 blend: the delta is 0xff-prefixed 16.16
        assert!(cs
            .bytes
            .windows(5)
            .any(|w| w == [0xff, 0x00, 0x64, 0x8f, 0x5c]));
    }

    /// Blends are batched until the CFF2 operand stack is full, and only the
    /// 513-operand CFF2 limit bounds the run.
    ///
    /// fontTools' `_convertToBlendCmds` on 60 arguments blending over 8
    /// regions returns two groups, of 56 and 4 arguments.
    #[test]
    fn long_blend_runs_are_split() {
        // one moveto plus 60 alternating h/v linetos, all varying
        let mut builder = Cff2CharstringBuilder::new(9);
        for master in 0..9 {
            let pen = builder.master(master);
            let mut pos = Point::new(0.0, 0.0);
            pen.move_to(pos);
            for i in 0..60 {
                let step = 10.0 + master as f64;
                if i % 2 == 0 {
                    pos.x += step;
                } else {
                    pos.y += step;
                }
                pen.line_to(pos);
            }
        }
        let cs = builder
            .build(|values| {
                let mut out = vec![values[0]];
                out.extend(values[1..].iter().map(|v| v - values[0]));
                out
            })
            .unwrap();
        // the operand before each `blend` is the number of arguments in the
        // run; 56 and 4 encode as single bytes
        assert!(cs.bytes.windows(2).any(|w| w == [139 + 56, opcode::BLEND]));
        assert!(cs.bytes.windows(2).any(|w| w == [139 + 4, opcode::BLEND]));
        assert_eq!(cs.bytes.iter().filter(|b| **b == opcode::BLEND).count(), 2);
    }
}

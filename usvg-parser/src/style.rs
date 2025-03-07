// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use rosvgtree::{self, AttributeId as AId};
use usvg_tree::{Color, Fill, FuzzyEq, Opacity, Paint, Stroke, StrokeMiterlimit, Units};

use crate::rosvgtree_ext::{FromValue, OpacityWrapper, SvgColorExt, SvgNodeExt2};
use crate::{converter, paint_server, SvgNodeExt};

impl<'a, 'input: 'a> FromValue<'a, 'input> for usvg_tree::LineCap {
    fn parse(_: rosvgtree::Node, _: rosvgtree::AttributeId, value: &str) -> Option<Self> {
        match value {
            "butt" => Some(usvg_tree::LineCap::Butt),
            "round" => Some(usvg_tree::LineCap::Round),
            "square" => Some(usvg_tree::LineCap::Square),
            _ => None,
        }
    }
}

impl<'a, 'input: 'a> FromValue<'a, 'input> for usvg_tree::LineJoin {
    fn parse(_: rosvgtree::Node, _: rosvgtree::AttributeId, value: &str) -> Option<Self> {
        match value {
            "miter" => Some(usvg_tree::LineJoin::Miter),
            "round" => Some(usvg_tree::LineJoin::Round),
            "bevel" => Some(usvg_tree::LineJoin::Bevel),
            _ => None,
        }
    }
}

impl<'a, 'input: 'a> FromValue<'a, 'input> for usvg_tree::FillRule {
    fn parse(_: rosvgtree::Node, _: rosvgtree::AttributeId, value: &str) -> Option<Self> {
        match value {
            "nonzero" => Some(usvg_tree::FillRule::NonZero),
            "evenodd" => Some(usvg_tree::FillRule::EvenOdd),
            _ => None,
        }
    }
}

pub(crate) fn resolve_fill(
    node: rosvgtree::Node,
    has_bbox: bool,
    state: &converter::State,
    cache: &mut converter::Cache,
) -> Option<Fill> {
    if state.parent_clip_path.is_some() {
        // A `clipPath` child can be filled only with a black color.
        return Some(Fill {
            paint: Paint::Color(Color::black()),
            opacity: Opacity::ONE,
            rule: node
                .find_and_parse_attribute(AId::ClipRule)
                .unwrap_or_default(),
        });
    }

    let mut sub_opacity = Opacity::ONE;
    let paint = if let Some(n) = node.ancestors().find(|n| n.has_attribute(AId::Fill)) {
        convert_paint(n, AId::Fill, has_bbox, state, &mut sub_opacity, cache)?
    } else {
        Paint::Color(Color::black())
    };

    let fill_opacity = node
        .parse_attribute::<OpacityWrapper>(AId::FillOpacity)
        .map(|v| v.0)
        .unwrap_or(Opacity::ONE);

    Some(Fill {
        paint,
        opacity: sub_opacity * fill_opacity,
        rule: node
            .find_and_parse_attribute(AId::FillRule)
            .unwrap_or_default(),
    })
}

pub(crate) fn resolve_stroke(
    node: rosvgtree::Node,
    has_bbox: bool,
    state: &converter::State,
    cache: &mut converter::Cache,
) -> Option<Stroke> {
    if state.parent_clip_path.is_some() {
        // A `clipPath` child cannot be stroked.
        return None;
    }

    let mut sub_opacity = Opacity::ONE;
    let paint = if let Some(n) = node.ancestors().find(|n| n.has_attribute(AId::Stroke)) {
        convert_paint(n, AId::Stroke, has_bbox, state, &mut sub_opacity, cache)?
    } else {
        return None;
    };

    let width = node.resolve_valid_length(AId::StrokeWidth, state, 1.0)?;

    // Must be bigger than 1.
    let miterlimit = node
        .find_and_parse_attribute(AId::StrokeMiterlimit)
        .unwrap_or(4.0);
    let miterlimit = if miterlimit < 1.0 { 1.0 } else { miterlimit };
    let miterlimit = StrokeMiterlimit::new(miterlimit);

    let stroke_opacity = node
        .parse_attribute::<OpacityWrapper>(AId::StrokeOpacity)
        .map(|v| v.0)
        .unwrap_or(Opacity::ONE);

    let stroke = Stroke {
        paint,
        dasharray: conv_dasharray(node, state),
        dashoffset: node.resolve_length(AId::StrokeDashoffset, state, 0.0) as f32,
        miterlimit,
        opacity: sub_opacity * stroke_opacity,
        width,
        linecap: node
            .find_and_parse_attribute(AId::StrokeLinecap)
            .unwrap_or_default(),
        linejoin: node
            .find_and_parse_attribute(AId::StrokeLinejoin)
            .unwrap_or_default(),
    };

    Some(stroke)
}

fn convert_paint(
    node: rosvgtree::Node,
    aid: AId,
    has_bbox: bool,
    state: &converter::State,
    opacity: &mut Opacity,
    cache: &mut converter::Cache,
) -> Option<Paint> {
    let value: &str = node.attribute(aid)?;
    let paint = match svgtypes::Paint::from_str(value) {
        Ok(v) => v,
        Err(_) => {
            if aid == AId::Fill {
                log::warn!(
                    "Failed to parse fill value: '{}'. Fallback to black.",
                    value
                );
                svgtypes::Paint::Color(svgtypes::Color::black())
            } else {
                return None;
            }
        }
    };

    match paint {
        svgtypes::Paint::None => None,
        svgtypes::Paint::Inherit => None, // already resolved by rosvgtree
        svgtypes::Paint::CurrentColor => {
            let svg_color: svgtypes::Color = node
                .find_and_parse_attribute(AId::Color)
                .unwrap_or_else(svgtypes::Color::black);
            let (color, alpha) = svg_color.split_alpha();
            *opacity = alpha;
            Some(Paint::Color(color))
        }
        svgtypes::Paint::Color(svg_color) => {
            let (color, alpha) = svg_color.split_alpha();
            *opacity = alpha;
            Some(Paint::Color(color))
        }
        svgtypes::Paint::FuncIRI(func_iri, fallback) => {
            if let Some(link) = node.document().element_by_id(func_iri) {
                let tag_name = link.tag_name().unwrap();
                if tag_name.is_paint_server() {
                    match paint_server::convert(link, state, cache) {
                        Some(paint_server::ServerOrColor::Server(paint)) => {
                            // We can use a paint server node with ObjectBoundingBox units
                            // for painting only when the shape itself has a bbox.
                            //
                            // See SVG spec 7.11 for details.
                            if !has_bbox && paint.units() == Some(Units::ObjectBoundingBox) {
                                from_fallback(node, fallback, opacity)
                            } else {
                                Some(paint)
                            }
                        }
                        Some(paint_server::ServerOrColor::Color { color, opacity: so }) => {
                            *opacity = so;
                            Some(Paint::Color(color))
                        }
                        None => from_fallback(node, fallback, opacity),
                    }
                } else {
                    log::warn!("'{}' cannot be used to {} a shape.", tag_name, aid);
                    None
                }
            } else {
                from_fallback(node, fallback, opacity)
            }
        }
    }
}

fn from_fallback(
    node: rosvgtree::Node,
    fallback: Option<svgtypes::PaintFallback>,
    opacity: &mut Opacity,
) -> Option<Paint> {
    match fallback? {
        svgtypes::PaintFallback::None => None,
        svgtypes::PaintFallback::CurrentColor => {
            let svg_color: svgtypes::Color = node
                .find_and_parse_attribute(AId::Color)
                .unwrap_or_else(svgtypes::Color::black);
            let (color, alpha) = svg_color.split_alpha();
            *opacity = alpha;
            Some(Paint::Color(color))
        }
        svgtypes::PaintFallback::Color(svg_color) => {
            let (color, alpha) = svg_color.split_alpha();
            *opacity = alpha;
            Some(Paint::Color(color))
        }
    }
}

// Prepare the 'stroke-dasharray' according to:
// https://www.w3.org/TR/SVG11/painting.html#StrokeDasharrayProperty
fn conv_dasharray(node: rosvgtree::Node, state: &converter::State) -> Option<Vec<f64>> {
    let node = node
        .ancestors()
        .find(|n| n.has_attribute(AId::StrokeDasharray))?;
    let list = super::units::convert_list(node, AId::StrokeDasharray, state)?;

    // `A negative value is an error`
    if list.iter().any(|n| n.is_sign_negative()) {
        return None;
    }

    // `If the sum of the values is zero, then the stroke is rendered
    // as if a value of none were specified.`
    {
        // no Iter::sum(), because of f64

        let mut sum = 0.0f64;
        for n in list.iter() {
            sum += *n;
        }

        if sum.fuzzy_eq(&0.0) {
            return None;
        }
    }

    // `If an odd number of values is provided, then the list of values
    // is repeated to yield an even number of values.`
    if list.len() % 2 != 0 {
        let mut tmp_list = list.clone();
        tmp_list.extend_from_slice(&list);
        return Some(tmp_list);
    }

    Some(list)
}

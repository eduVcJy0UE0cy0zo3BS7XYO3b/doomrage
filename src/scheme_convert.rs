use crate::render::{DrawCmd, PlotData, RenderBlock};
use scheme_rs::value::{UnpackedValue, Value};

/// Collect a Scheme list (pair chain ending in null) into a Vec<Value>
pub(crate) fn collect_list(val: &Value) -> Vec<Value> {
    let mut result = Vec::new();
    let mut current = val.clone();
    loop {
        match current.clone().unpack() {
            UnpackedValue::Pair(p) => {
                result.push(p.car());
                current = p.cdr();
            }
            UnpackedValue::Null => break,
            _ => {
                result.push(current);
                break;
            }
        }
    }
    result
}

/// Collect list elements from a Pair (avoids extra clone of first Value)
pub(crate) fn collect_list_from_pair(pair: &scheme_rs::lists::Pair) -> Vec<Value> {
    let mut result = vec![pair.car()];
    let mut current = pair.cdr();
    loop {
        match current.clone().unpack() {
            UnpackedValue::Pair(p) => {
                result.push(p.car());
                current = p.cdr();
            }
            UnpackedValue::Null => break,
            _ => {
                result.push(current);
                break;
            }
        }
    }
    result
}

/// Convert a Scheme Value to a serde_json::Value by walking the Value tree.
/// Recognizes:
///   - Number → Number
///   - String → String
///   - #t/#f → Bool
///   - null → Null
///   - symbol json-null → Null
///   - (json-object (key . val) ...) → Object
///   - (list ...) → Array
pub fn scheme_value_to_json(val: &Value) -> serde_json::Value {
    use serde_json::Value as JV;

    // Try number first
    if let Some(f) = val.cast_to_scheme_type::<f64>() {
        return serde_json::Number::from_f64(f)
            .map(JV::Number)
            .unwrap_or(JV::Null);
    }

    match val.clone().unpack() {
        UnpackedValue::Boolean(b) => JV::Bool(b),
        UnpackedValue::Null => JV::Null,
        UnpackedValue::String(s) => JV::String(String::from(s)),
        UnpackedValue::Symbol(s) => {
            let name = format!("{}", s);
            if name == "json-null" {
                JV::Null
            } else {
                JV::String(name)
            }
        }
        UnpackedValue::Pair(p) => {
            let items = collect_list_from_pair(&p);

            // Check if first element is the symbol 'json-object
            if let Some(first) = items.first() {
                let first_str = format!("{}", first);
                if first_str == "json-object" {
                    // Reconstruct a JSON object from (cons "key" value) pairs
                    let mut map = serde_json::Map::new();
                    for item in &items[1..] {
                        if let UnpackedValue::Pair(kv) = item.clone().unpack() {
                            let key = format!("{}", kv.car());
                            let val = scheme_value_to_json(&kv.cdr());
                            map.insert(key, val);
                        }
                    }
                    return JV::Object(map);
                }
            }

            // Regular list → JSON array
            JV::Array(items.iter().map(scheme_value_to_json).collect())
        }
        _ => {
            // Fallback: display as string
            JV::String(format!("{}", val))
        }
    }
}

/// Extract display string from a Scheme Value
fn value_display(val: &Value) -> String {
    match val.clone().unpack() {
        UnpackedValue::String(s) => String::from(s),
        _ => format!("{}", val),
    }
}

/// Parse render blocks directly from Scheme Value tree (no string round-trip)
pub(crate) fn try_parse_render_from_value(val: &Value) -> Option<Vec<RenderBlock>> {
    let pair = match val.clone().unpack() {
        UnpackedValue::Pair(p) => p,
        _ => return None,
    };
    let tag = format!("{}", pair.car());
    let args = collect_list(&pair.cdr());

    match tag.as_str() {
        "render-text" => Some(vec![RenderBlock::Text(value_display(&args[0]))]),
        "render-bold" => Some(vec![RenderBlock::Bold(value_display(&args[0]))]),
        "render-italic" => Some(vec![RenderBlock::Italic(value_display(&args[0]))]),
        "render-code" => Some(vec![RenderBlock::Code(value_display(&args[0]))]),
        "render-link" => Some(vec![RenderBlock::Link {
            url: value_display(&args[0]),
            label: value_display(&args[1]),
        }]),
        "render-hr" => Some(vec![RenderBlock::Hr]),
        "render-table" => {
            let headers = collect_list(&args[0]).iter().map(|v| value_display(v)).collect();
            let rows = collect_list(&args[1])
                .iter()
                .map(|row| collect_list(row).iter().map(|v| value_display(v)).collect())
                .collect();
            Some(vec![RenderBlock::Table { headers, rows }])
        }
        "render-plot-line" => {
            let data = collect_list(&args[0])
                .iter()
                .filter_map(|v| v.cast_to_scheme_type::<f64>())
                .collect();
            let title = args.get(1).map(|v| value_display(v)).unwrap_or_default();
            Some(vec![RenderBlock::Plot(PlotData::Line {
                y: data,
                title: if title.is_empty() { None } else { Some(title) },
            })])
        }
        "render-plot-scatter" => {
            let xs = collect_list(&args[0])
                .iter()
                .filter_map(|v| v.cast_to_scheme_type::<f64>())
                .collect();
            let ys = collect_list(&args[1])
                .iter()
                .filter_map(|v| v.cast_to_scheme_type::<f64>())
                .collect();
            let title = args.get(2).map(|v| value_display(v)).unwrap_or_default();
            Some(vec![RenderBlock::Plot(PlotData::Scatter {
                x: xs,
                y: ys,
                title: if title.is_empty() { None } else { Some(title) },
            })])
        }
        "render-plot-bar" => {
            let labels = collect_list(&args[0]).iter().map(|v| value_display(v)).collect();
            let values = collect_list(&args[1])
                .iter()
                .filter_map(|v| v.cast_to_scheme_type::<f64>())
                .collect();
            let title = args.get(2).map(|v| value_display(v)).unwrap_or_default();
            Some(vec![RenderBlock::Plot(PlotData::Bar {
                labels,
                values,
                title: if title.is_empty() { None } else { Some(title) },
            })])
        }
        "render-group" => {
            let items = collect_list(&args[0]);
            let blocks = items
                .iter()
                .filter_map(try_parse_render_from_value)
                .flatten()
                .collect();
            Some(blocks)
        }
        "render-button" => {
            let label = value_display(&args[0]);
            let action_type = value_display(&args[1]);
            let action_args = args.get(2).map(|a| collect_list(a)).unwrap_or_default();
            let arg1 = action_args.first().map(|v| value_display(v)).unwrap_or_default();
            let arg2 = action_args.get(1).map(|v| value_display(v)).unwrap_or_default();
            let action = match action_type.as_str() {
                "append" => crate::render::StoreAction::Append { key: arg1, value: arg2 },
                "delete" => crate::render::StoreAction::Delete { key: arg1 },
                _ => crate::render::StoreAction::Set { key: arg1, value: arg2 },
            };
            Some(vec![RenderBlock::Button { label, action }])
        }
        "render-checkbox" => {
            Some(vec![RenderBlock::Checkbox {
                label: value_display(&args[0]),
                key: value_display(&args[1]),
            }])
        }
        "render-text-input" => {
            Some(vec![RenderBlock::TextInput {
                key: value_display(&args[0]),
                placeholder: args.get(1).map(|v| value_display(v)).unwrap_or_default(),
            }])
        }
        "render-slider" => {
            Some(vec![RenderBlock::Slider {
                key: value_display(&args[0]),
                min: args.get(1).and_then(|v| v.cast_to_scheme_type::<f64>()).unwrap_or(0.0),
                max: args.get(2).and_then(|v| v.cast_to_scheme_type::<f64>()).unwrap_or(100.0),
            }])
        }
        "render-editable-list" => {
            Some(vec![RenderBlock::EditableList {
                key: value_display(&args[0]),
            }])
        }
        "render-canvas" => {
            let width = args.get(0).and_then(|v| v.cast_to_scheme_type::<f64>()).unwrap_or(200.0);
            let height = args.get(1).and_then(|v| v.cast_to_scheme_type::<f64>()).unwrap_or(150.0);
            let cmd_list = args.get(2).map(|v| collect_list(v)).unwrap_or_default();
            let commands = cmd_list.iter().filter_map(|cmd| parse_draw_cmd(cmd)).collect();
            Some(vec![RenderBlock::Canvas { width, height, commands }])
        }
        _ => None,
    }
}

fn parse_draw_cmd(val: &Value) -> Option<DrawCmd> {
    let items = collect_list(val);
    let tag = format!("{}", items.first()?);
    let f = |i: usize| items.get(i).and_then(|v| v.cast_to_scheme_type::<f64>()).unwrap_or(0.0);
    let s = |i: usize| items.get(i).map(|v| value_display(v)).unwrap_or_default();

    match tag.as_str() {
        "line" => Some(DrawCmd::Line { x1: f(1), y1: f(2), x2: f(3), y2: f(4), color: s(5), width: f(6) }),
        "rect" => Some(DrawCmd::Rect { x: f(1), y: f(2), w: f(3), h: f(4), fill: s(5) }),
        "circle" => Some(DrawCmd::Circle { x: f(1), y: f(2), r: f(3), fill: s(4) }),
        "polyline" => {
            let pts_list = items.get(1).map(|v| collect_list(v)).unwrap_or_default();
            let points: Vec<[f64; 2]> = pts_list.iter().map(|pt| {
                let coords = collect_list(pt);
                let x = coords.first().and_then(|v| v.cast_to_scheme_type::<f64>()).unwrap_or(0.0);
                let y = coords.get(1).and_then(|v| v.cast_to_scheme_type::<f64>()).unwrap_or(0.0);
                [x, y]
            }).collect();
            Some(DrawCmd::Polyline { points, color: s(2), width: f(3) })
        }
        "text" => Some(DrawCmd::Text { x: f(1), y: f(2), text: s(3), color: s(4), size: f(5) }),
        _ => None,
    }
}

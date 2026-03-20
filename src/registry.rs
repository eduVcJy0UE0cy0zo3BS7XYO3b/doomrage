use crate::types::*;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct NodeRegistry {
    pub templates: HashMap<String, NodeTemplate>,
    pub nodes_dir: PathBuf,
}

impl NodeRegistry {
    pub fn new(nodes_dir: PathBuf) -> Self {
        Self {
            templates: HashMap::new(),
            nodes_dir,
        }
    }

    pub fn scan(&mut self) -> Result<()> {
        self.register_builtins();

        if !self.nodes_dir.exists() {
            std::fs::create_dir_all(&self.nodes_dir)?;
            return Ok(());
        }

        self.scan_dir(&self.nodes_dir.clone(), "")?;
        Ok(())
    }

    fn scan_dir(&mut self, dir: &Path, category: &str) -> Result<()> {
        let entries = std::fs::read_dir(dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let subcat = if category.is_empty() {
                    path.file_name().unwrap().to_string_lossy().to_string()
                } else {
                    format!("{}/{}", category, path.file_name().unwrap().to_string_lossy())
                };
                self.scan_dir(&path, &subcat)?;
            } else if path.extension().map_or(false, |e| e == "wit") {
                if let Err(e) = self.load_wit_node(&path, category) {
                    log::warn!("Failed to load node from {:?}: {}", path, e);
                }
            }
        }
        Ok(())
    }

    fn load_wit_node(&mut self, wit_path: &Path, category: &str) -> Result<()> {
        let stem = wit_path.file_stem().unwrap().to_string_lossy().to_string();
        let wasm_path = wit_path.with_extension("wasm");

        let wit_text = std::fs::read_to_string(wit_path)
            .with_context(|| format!("Reading {}", wit_path.display()))?;

        let (inputs, outputs) = parse_wit_ports(&wit_text)
            .with_context(|| format!("Parsing WIT from {}", wit_path.display()))?;

        let wasm_bytes = if wasm_path.exists() {
            Some(std::fs::read(&wasm_path)?)
        } else {
            log::warn!("No .wasm file for node '{}' at {:?}", stem, wasm_path);
            None
        };

        let template = NodeTemplate {
            name: stem.clone(),
            category: category.to_string(),
            path: Some(wit_path.to_path_buf()),
            inputs,
            outputs,
            wasm_bytes,
            builtin: None,
        };

        self.templates.insert(stem, template);
        Ok(())
    }

    pub fn reload_wasm(&mut self, wasm_path: &Path) -> Result<()> {
        let stem = wasm_path.file_stem().unwrap().to_string_lossy().to_string();
        if let Some(template) = self.templates.get_mut(&stem) {
            let bytes = std::fs::read(wasm_path)?;
            template.wasm_bytes = Some(bytes);
            log::info!("Reloaded WASM for node '{}'", stem);
        }
        Ok(())
    }

    fn register_builtins(&mut self) {
        self.templates.insert(
            "Const".to_string(),
            NodeTemplate {
                name: "Const".to_string(),
                category: "Built-in".to_string(),
                path: None,
                inputs: vec![],
                outputs: vec![PortDef {
                    name: "out".to_string(),
                    port_type: PortType::F64,
                }],
                wasm_bytes: None,
                builtin: Some(BuiltinKind::Const),
            },
        );

        self.templates.insert(
            "Output".to_string(),
            NodeTemplate {
                name: "Output".to_string(),
                category: "Built-in".to_string(),
                path: None,
                inputs: vec![PortDef {
                    name: "in".to_string(),
                    port_type: PortType::F64,
                }],
                outputs: vec![],
                wasm_bytes: None,
                builtin: Some(BuiltinKind::Output),
            },
        );

        self.templates.insert(
            "Script".to_string(),
            NodeTemplate {
                name: "Script".to_string(),
                category: "Built-in".to_string(),
                path: None,
                inputs: vec![],
                outputs: vec![],
                wasm_bytes: None,
                builtin: Some(BuiltinKind::Script),
            },
        );
    }

    pub fn grouped_templates(&self) -> Vec<(String, Vec<&NodeTemplate>)> {
        let mut groups: HashMap<String, Vec<&NodeTemplate>> = HashMap::new();
        for t in self.templates.values() {
            groups
                .entry(t.category.clone())
                .or_default()
                .push(t);
        }
        let mut result: Vec<_> = groups.into_iter().collect();
        result.sort_by(|a, b| {
            let order = |s: &str| match s {
                "Built-in" => 0,
                _ => 1,
            };
            order(&a.0).cmp(&order(&b.0)).then(a.0.cmp(&b.0))
        });
        for (_, templates) in &mut result {
            templates.sort_by(|a, b| a.name.cmp(&b.name));
        }
        result
    }
}

fn parse_wit_ports(wit_text: &str) -> Result<(Vec<PortDef>, Vec<PortDef>)> {
    // Parse the WIT to find the `run` export function signature.
    // We use a simple text parser since wit-parser crate expects a full package resolution.
    // WIT format: `export run: func(a: f64, b: f64) -> f64;`

    let mut inputs = Vec::new();
    let mut outputs = Vec::new();

    for line in wit_text.lines() {
        let line = line.trim();
        if !line.contains("func(") {
            continue;
        }

        // Extract function name (we look for "run" or any exported func)
        let is_export = line.starts_with("export ");

        if !is_export {
            continue;
        }

        // Extract params between ( and )
        if let Some(params_start) = line.find('(') {
            if let Some(params_end) = line.find(')') {
                let params_str = &line[params_start + 1..params_end];
                if !params_str.trim().is_empty() {
                    for param in params_str.split(',') {
                        let param = param.trim();
                        if let Some((name, typ)) = param.split_once(':') {
                            let name = name.trim().to_string();
                            let port_type = parse_wit_type(typ.trim())?;
                            inputs.push(PortDef { name, port_type });
                        }
                    }
                }
            }
        }

        // Extract return type after ->
        if let Some(arrow_pos) = line.find("->") {
            let ret_str = line[arrow_pos + 2..].trim().trim_end_matches(';').trim();
            if ret_str.starts_with('(') && ret_str.ends_with(')') {
                // Tuple return: (f64, f64)
                let inner = &ret_str[1..ret_str.len() - 1];
                for (i, typ) in inner.split(',').enumerate() {
                    let port_type = parse_wit_type(typ.trim())?;
                    outputs.push(PortDef {
                        name: format!("out{}", i),
                        port_type,
                    });
                }
            } else {
                let port_type = parse_wit_type(ret_str)?;
                outputs.push(PortDef {
                    name: "out".to_string(),
                    port_type,
                });
            }
        }

        break; // Only parse first export func
    }

    Ok((inputs, outputs))
}

fn parse_wit_type(s: &str) -> Result<PortType> {
    match s {
        "f64" => Ok(PortType::F64),
        "f32" => Ok(PortType::F32),
        "s64" | "i64" => Ok(PortType::I64),
        "s32" | "i32" => Ok(PortType::I32),
        "bool" => Ok(PortType::Bool),
        "string" => Ok(PortType::Str),
        other => anyhow::bail!("Unsupported WIT type: {}", other),
    }
}

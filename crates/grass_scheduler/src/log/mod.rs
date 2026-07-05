//! Schedule text and DOT output helpers.

use crate::{Scheduler, StoredSystemEntry, SystemGroupInfo};
use std::collections::HashMap;

impl Scheduler {
    fn short_name(full: &str) -> String {
        let parts: Vec<&str> = full.rsplitn(3, "::").collect();
        match parts.len() {
            0 => full.to_string(),
            1 => parts[0].to_string(),
            _ => format!("{}::{}", parts[1], parts[0]),
        }
    }

    fn condition_short_name(full: &str) -> String {
        // in_state closure: "grass_scheduler::in_state::{{closure}}" → "in_state(..)"
        if full.contains("in_state") && full.contains("{{closure}}") {
            return "in_state(..)".to_string();
        }
        // first_stage_only closure: "grass_scheduler::first_stage_only::{{closure}}" → "first_stage_only()"
        if full.contains("first_stage_only") && full.contains("{{closure}}") {
            return "first_stage_only()".to_string();
        }
        // in_stage closure
        if full.contains("in_stage") && full.contains("{{closure}}") {
            return "in_stage(..)".to_string();
        }
        // Generic closure fallback
        if full.contains("{{closure}}") {
            // Try to extract the function name before ::{{closure}}
            if let Some(pos) = full.rfind("::{{closure}}") {
                let prefix = &full[..pos];
                if let Some(last_sep) = prefix.rfind("::") {
                    return format!("{}(..)", &prefix[last_sep + 2..]);
                }
                return format!("{}(..)", prefix);
            }
        }
        Self::short_name(full)
    }

    /// Prints the full schedule to stdout, grouped by `ScheduleSetupSet` and [`crate::ScheduleSet`].
    pub fn print_schedule(&self) {
        println!("\n═══ Setup Systems ═══");
        let mut last_key: Option<(u32, u32)> = None;
        for (entry, phase) in &self.setup_systems {
            if last_key != Some(phase.sort_key()) {
                println!("  [{}]", phase.phase_name());
                last_key = Some(phase.sort_key());
            }
            let short = Self::short_name(&entry.name);
            if let Some(lbl) = &entry.label {
                print!("    {}  [{}]", short, lbl);
            } else {
                print!("    {}", short);
            }
            if !entry.afters.is_empty() {
                print!("  after: {}", entry.afters.join(", "));
            }
            if !entry.befores.is_empty() {
                print!("  before: {}", entry.befores.join(", "));
            }
            if let Some(cond) = &entry.condition_name {
                print!("  run_if: {}", Self::condition_short_name(cond));
            }
            println!();
        }

        println!("\n═══ Update Systems (per-step) ═══");
        let mut last_key: Option<(u32, u32)> = None;
        for (entry, phase) in &self.update_systems {
            if last_key != Some(phase.sort_key()) {
                println!("  [{}]", phase.phase_name());
                last_key = Some(phase.sort_key());
            }
            let short = Self::short_name(&entry.name);
            if let Some(lbl) = &entry.label {
                print!("    {}  [{}]", short, lbl);
            } else {
                print!("    {}", short);
            }
            if !entry.afters.is_empty() {
                print!("  after: {}", entry.afters.join(", "));
            }
            if !entry.befores.is_empty() {
                print!("  before: {}", entry.befores.join(", "));
            }
            if let Some(cond) = &entry.condition_name {
                print!("  run_if: {}", Self::condition_short_name(cond));
            }
            println!();
        }
        println!();
    }

    /// Returns the stage index a condition belongs to, or `None` (= show in all stages).
    fn condition_stage_index(&self, cond_name: &str) -> Option<usize> {
        Self::condition_stage_index_static(cond_name, &self.stage_names)
    }

    fn condition_stage_index_static(cond_name: &str, stage_names: &[String]) -> Option<usize> {
        if stage_names.is_empty() {
            return None;
        }
        // "in_state(Insert)" → match variant name to stage name (case-insensitive)
        if let Some(variant) = cond_name
            .strip_prefix("in_state(")
            .and_then(|s| s.strip_suffix(')'))
        {
            let variant_lower = variant.to_lowercase();
            return stage_names
                .iter()
                .position(|s| s.to_lowercase() == variant_lower);
        }
        // "in_stage(relax)" → exact match
        if let Some(name) = cond_name
            .strip_prefix("in_stage(")
            .and_then(|s| s.strip_suffix(')'))
        {
            return stage_names.iter().position(|s| s == name);
        }
        // "first_stage_only()" → first stage
        if cond_name == "first_stage_only()" {
            return Some(0);
        }
        None
    }

    /// Returns true if an update system should appear in the given stage.
    fn system_visible_in_stage(&self, entry: &StoredSystemEntry, stage_idx: usize) -> bool {
        match &entry.condition_name {
            None => true, // unconditional → visible in all stages
            Some(cond) => {
                match self.condition_stage_index(cond) {
                    Some(idx) => idx == stage_idx, // stage-specific → only in matching stage
                    None => true,                  // unknown condition → show in all stages
                }
            }
        }
    }

    /// Returns true if a setup system should appear in the given stage.
    /// Setup runs every stage, so unconditional setup systems are visible in all stages.
    fn setup_visible_in_stage(&self, entry: &StoredSystemEntry, stage_idx: usize) -> bool {
        self.system_visible_in_stage(entry, stage_idx)
    }

    /// Writes the organized schedule to `path` as a Graphviz DOT file.
    ///
    /// Render with `dot -Tpng schedule.dot -o schedule.png`. Usually invoked
    /// indirectly via [`Scheduler::enable_schedule_print`] rather than directly.
    pub fn write_dot(&self, path: &str) {
        use std::io::Write;
        let mut out = String::new();
        out.push_str("digraph schedule {\n");
        out.push_str("    node [shape=box, style=filled, fillcolor=lightyellow];\n\n");

        // Helper to make valid DOT node IDs
        let node_id = |prefix: &str, idx: usize| -> String { format!("{}_{}", prefix, idx) };

        // Capture stage_names for use in closures
        let stage_names = &self.stage_names;

        // Check if a condition matches a specific stage index
        let cond_matches_stage = |cond: &str, si: usize| -> bool {
            Self::condition_stage_index_static(cond, stage_names) == Some(si)
        };

        // Helper to build a node label with optional condition annotation.
        // When `current_stage` is Some, conditions that match that stage are suppressed
        // (e.g. first_stage_only() in stage 0 is redundant).
        let make_label = |entry: &StoredSystemEntry, current_stage: Option<usize>| -> String {
            let short = Self::short_name(&entry.name);
            let mut label = if let Some(lbl) = &entry.label {
                format!("{}\\n[{}]", short, lbl)
            } else {
                short.to_string()
            };
            if let Some(cond) = &entry.condition_name {
                let show_cond = match current_stage {
                    Some(si) => !cond_matches_stage(cond, si),
                    None => true,
                };
                if show_cond {
                    label.push_str(&format!("\\nrun_if: {}", Self::condition_short_name(cond)));
                }
            }
            label
        };

        // Helper to build node style (dashed border for conditional systems).
        // Suppresses dashed border when the condition matches the current stage.
        let node_style = |entry: &StoredSystemEntry, current_stage: Option<usize>| -> String {
            let is_conditional = match &entry.condition_name {
                Some(cond) => match current_stage {
                    Some(si) => !cond_matches_stage(cond, si),
                    None => true,
                },
                None => false,
            };
            if is_conditional {
                "style=\"filled,dashed\"".to_string()
            } else {
                "style=filled".to_string()
            }
        };

        // Build setup groups once
        let mut setup_groups: Vec<(String, Vec<(usize, &StoredSystemEntry)>)> = Vec::new();
        for (i, (entry, phase)) in self.setup_systems.iter().enumerate() {
            let set_name = phase.phase_name().to_string();
            if let Some(last) = setup_groups.last_mut() {
                if last.0 == set_name {
                    last.1.push((i, entry));
                    continue;
                }
            }
            setup_groups.push((set_name, vec![(i, entry)]));
        }

        if self.stage_names.is_empty() {
            // No stages — single-loop layout with TB
            out.insert_str("digraph schedule {\n".len(), "    rankdir=TB;\n");
            self.write_dot_flat(&mut out, &node_id, &make_label, &node_style, &setup_groups);
        } else {
            // Per-stage layout: stages left-to-right, pipelines top-to-bottom
            out.insert_str(
                "digraph schedule {\n".len(),
                "    rankdir=TB;\n    newrank=true;\n",
            );
            self.write_dot_per_stage(&mut out, &node_id, &make_label, &node_style, &setup_groups);
        }

        // Legend
        out.push_str("    subgraph cluster_legend {\n");
        out.push_str("        label=\"Legend\";\n");
        out.push_str("        style=filled; fillcolor=white;\n");
        out.push_str("        node [shape=plaintext, style=\"\", fillcolor=white];\n");
        out.push_str("        legend_1 [label=\"Blue bold = execution order\"];\n");
        out.push_str("        legend_2 [label=\"Red dashed = before/after constraint\"];\n");
        out.push_str("        legend_3 [label=\"Purple dotted = requires constraint\"];\n");
        out.push_str("        legend_4 [label=\"Green bold = run loop\"];\n");
        out.push_str("        legend_5 [label=\"Dashed border = conditional (run_if)\"];\n");
        out.push_str(
            "        legend_1 -> legend_2 -> legend_3 -> legend_4 -> legend_5 [style=invis];\n",
        );
        out.push_str("    }\n\n");

        out.push_str("}\n");

        let mut file = std::fs::File::create(path).expect("Failed to create DOT file");
        file.write_all(out.as_bytes())
            .expect("Failed to write DOT file");
        println!("Schedule DOT file written to: {}", path);
    }

    /// Emits a DOT subgraph cluster for a [`SystemGroup`], with inner phase sub-clusters,
    /// execution edges, and an optional loop back-edge.
    fn emit_group_subgraph(
        out: &mut String,
        info: &SystemGroupInfo,
        entry: &StoredSystemEntry,
        idx: usize,
        prefix: &str,
        current_stage: Option<usize>,
        make_label: &dyn Fn(&StoredSystemEntry, Option<usize>) -> String,
        indent: &str,
    ) {
        let _ = (current_stage, make_label);
        let gprefix = format!("{}_{}_g", prefix, idx);
        let group_name = &entry.name;

        // Group label with loop info
        let mut glabel = group_name.clone();
        if let Some(cond) = &info.loop_condition_name {
            let max = info.max_iterations.unwrap_or(0);
            glabel.push_str(&format!(
                "\\nloop while {} (max {})",
                Self::short_name(cond),
                max
            ));
        }

        out.push_str(&format!(
            "{}subgraph cluster_group_{}_{} {{\n",
            indent, prefix, idx
        ));
        out.push_str(&format!("{}    label=\"{}\";\n", indent, glabel));
        out.push_str(&format!(
            "{}    style=filled; fillcolor=\"#E0F7FA\";\n",
            indent
        ));

        // Group inner systems by phase name for sub-clusters
        let mut phase_groups: Vec<(&str, Vec<usize>)> = Vec::new();
        for (j, (_sys_name, phase_name)) in info.inner_systems.iter().enumerate() {
            if let Some(last) = phase_groups.last_mut() {
                if last.0 == phase_name.as_str() {
                    last.1.push(j);
                    continue;
                }
            }
            phase_groups.push((phase_name.as_str(), vec![j]));
        }

        for (phase_name, indices) in &phase_groups {
            out.push_str(&format!(
                "{}    subgraph cluster_group_{}_{}_{} {{\n",
                indent, prefix, idx, phase_name
            ));
            out.push_str(&format!("{}        label=\"{}\";\n", indent, phase_name));
            out.push_str(&format!(
                "{}        style=filled; fillcolor=lightyellow;\n",
                indent
            ));
            for &j in indices {
                let short = Self::short_name(&info.inner_systems[j].0);
                out.push_str(&format!(
                    "{}        {}{} [label=\"{}\", style=filled, fillcolor=lightyellow];\n",
                    indent, gprefix, j, short
                ));
            }
            out.push_str(&format!("{}    }}\n", indent));
        }

        // Execution edges between consecutive inner systems
        for j in 0..info.inner_systems.len().saturating_sub(1) {
            out.push_str(&format!(
                "{}    {}{} -> {}{} [color=blue, style=bold];\n",
                indent,
                gprefix,
                j,
                gprefix,
                j + 1
            ));
        }

        // Loop back-edge
        if info.loop_condition_name.is_some() && !info.inner_systems.is_empty() {
            let last_j = info.inner_systems.len() - 1;
            let cond_short = info
                .loop_condition_name
                .as_ref()
                .map(|c| Self::short_name(c))
                .unwrap_or_default();
            out.push_str(&format!(
                "{}    {}{} -> {}0 [color=green, style=bold, label=\"loop while {}\"];\n",
                indent, gprefix, last_j, gprefix, cond_short
            ));
        }

        out.push_str(&format!("{}}}\n", indent));
    }

    /// Flat single-loop DOT layout (no stages). Setup shown once, then one run loop.
    fn write_dot_flat(
        &self,
        out: &mut String,
        node_id: &dyn Fn(&str, usize) -> String,
        make_label: &dyn Fn(&StoredSystemEntry, Option<usize>) -> String,
        node_style: &dyn Fn(&StoredSystemEntry, Option<usize>) -> String,
        setup_groups: &[(String, Vec<(usize, &StoredSystemEntry)>)],
    ) {
        // Setup systems
        for (set_name, entries) in setup_groups {
            out.push_str(&format!("    subgraph cluster_setup_{} {{\n", set_name));
            out.push_str(&format!("        label=\"Setup: {}\";\n", set_name));
            out.push_str("        style=filled; fillcolor=lightblue;\n");
            for &(i, entry) in entries {
                out.push_str(&format!(
                    "        {} [label=\"{}\", {}];\n",
                    node_id("setup", i),
                    make_label(entry, None),
                    node_style(entry, None)
                ));
            }
            out.push_str("    }\n\n");
        }
        for i in 0..setup_groups.len().saturating_sub(1) {
            let tail = node_id("setup", setup_groups[i].1.last().unwrap().0);
            let head = node_id("setup", setup_groups[i + 1].1.first().unwrap().0);
            out.push_str(&format!(
                "    {} -> {} [color=blue, style=bold];\n",
                tail, head
            ));
        }
        for (_set_name, entries) in setup_groups {
            for w in entries.windows(2) {
                out.push_str(&format!(
                    "    {} -> {} [color=blue, style=bold];\n",
                    node_id("setup", w[0].0),
                    node_id("setup", w[1].0)
                ));
            }
        }

        // Update systems
        let mut update_groups: Vec<(String, Vec<(usize, &StoredSystemEntry)>)> = Vec::new();
        for (i, (entry, phase)) in self.update_systems.iter().enumerate() {
            let set_name = phase.phase_name().to_string();
            if let Some(last) = update_groups.last_mut() {
                if last.0 == set_name {
                    last.1.push((i, entry));
                    continue;
                }
            }
            update_groups.push((set_name, vec![(i, entry)]));
        }

        // Build first/last node ID maps (groups have different first/last nodes)
        let mut first_id: HashMap<usize, String> = HashMap::new();
        let mut last_id: HashMap<usize, String> = HashMap::new();

        for (set_name, entries) in &update_groups {
            out.push_str(&format!("    subgraph cluster_{} {{\n", set_name));
            out.push_str(&format!("        label=\"{}\";\n", set_name));
            out.push_str("        style=filled; fillcolor=lightyellow;\n");
            for &(i, entry) in entries {
                if let Some(info) = entry.system.group_info() {
                    Self::emit_group_subgraph(
                        out, info, entry, i, "update", None, make_label, "        ",
                    );
                    if !info.inner_systems.is_empty() {
                        first_id.insert(i, format!("update_{}_g0", i));
                        last_id
                            .insert(i, format!("update_{}_g{}", i, info.inner_systems.len() - 1));
                    } else {
                        let nid = node_id("update", i);
                        first_id.insert(i, nid.clone());
                        last_id.insert(i, nid);
                    }
                } else {
                    let nid = node_id("update", i);
                    out.push_str(&format!(
                        "        {} [label=\"{}\", {}];\n",
                        nid,
                        make_label(entry, None),
                        node_style(entry, None)
                    ));
                    first_id.insert(i, nid.clone());
                    last_id.insert(i, nid);
                }
            }
            out.push_str("    }\n\n");
        }

        for (_set_name, entries) in &update_groups {
            for w in entries.windows(2) {
                out.push_str(&format!(
                    "    {} -> {} [color=blue, style=bold];\n",
                    last_id[&w[0].0], first_id[&w[1].0]
                ));
            }
        }

        // Before/after constraint edges (use first_id for targets)
        let mut label_to_first: HashMap<String, String> = HashMap::new();
        for (i, (entry, _)) in self.update_systems.iter().enumerate() {
            if let Some(lbl) = &entry.label {
                label_to_first.insert(lbl.clone(), first_id[&i].clone());
            }
        }
        for (i, (entry, _)) in self.update_systems.iter().enumerate() {
            let from = &first_id[&i];
            for b in &entry.befores {
                if let Some(target) = label_to_first.get(b) {
                    out.push_str(&format!(
                        "    {} -> {} [color=red, style=dashed, label=\"before\"];\n",
                        from, target
                    ));
                }
            }
            for a in &entry.afters {
                if let Some(source) = label_to_first.get(a) {
                    out.push_str(&format!(
                        "    {} -> {} [color=red, style=dashed, label=\"after\"];\n",
                        source, from
                    ));
                }
            }
        }
        for (i, (entry, _)) in self.update_systems.iter().enumerate() {
            let from = &first_id[&i];
            for req in &entry.requires {
                if let Some(target) = label_to_first.get(req) {
                    out.push_str(&format!(
                        "    {} -> {} [color=purple, style=dotted, label=\"requires\", constraint=false];\n",
                        from, target
                    ));
                }
            }
        }

        // Cluster-to-cluster edges
        let tails: Vec<&String> = update_groups
            .iter()
            .map(|(_, e)| &last_id[&e.last().unwrap().0])
            .collect();
        let heads: Vec<&String> = update_groups
            .iter()
            .map(|(_, e)| &first_id[&e.first().unwrap().0])
            .collect();
        for i in 0..tails.len().saturating_sub(1) {
            out.push_str(&format!(
                "    {} -> {} [color=blue, style=bold];\n",
                tails[i],
                heads[i + 1]
            ));
        }
        if tails.len() >= 2 {
            out.push_str(&format!(
                "    {} -> {} [color=green, style=bold, label=\"run loop\", constraint=false];\n",
                tails.last().unwrap(),
                heads.first().unwrap()
            ));
        }
        if !setup_groups.is_empty() && !heads.is_empty() {
            let last_setup = setup_groups.last().unwrap();
            let last_setup_node = node_id("setup", last_setup.1.last().unwrap().0);
            out.push_str(&format!(
                "    {} -> {} [color=blue, style=bold, label=\"start run\"];\n",
                last_setup_node, heads[0]
            ));
        }
    }

    /// Per-stage DOT layout: stages left-to-right, each with Setup → Run pipeline top-to-bottom.
    fn write_dot_per_stage(
        &self,
        out: &mut String,
        node_id: &dyn Fn(&str, usize) -> String,
        make_label: &dyn Fn(&StoredSystemEntry, Option<usize>) -> String,
        node_style: &dyn Fn(&StoredSystemEntry, Option<usize>) -> String,
        _setup_groups: &[(String, Vec<(usize, &StoredSystemEntry)>)],
    ) {
        let stage_colors = [
            "#E8F5E9", "#E3F2FD", "#FFF3E0", "#F3E5F5", "#FFFDE7", "#FCE4EC",
        ];

        // Collect first-node IDs from each stage for rank=same alignment
        let mut stage_first_nodes: Vec<String> = Vec::new();

        for (si, stage_name) in self.stage_names.iter().enumerate() {
            let prefix = format!("s{}", si);
            let fill = stage_colors[si % stage_colors.len()];

            out.push_str(&format!("    subgraph cluster_stage_{} {{\n", si));
            out.push_str(&format!("        label=\"Stage: {}\";\n", stage_name));
            out.push_str(&format!("        style=filled; fillcolor=\"{}\";\n", fill));
            out.push_str("        color=darkgreen; penwidth=2;\n");

            // ── Setup systems within this stage ──────────────────────────
            let mut stage_setup_groups: Vec<(String, Vec<(usize, &StoredSystemEntry)>)> =
                Vec::new();
            for (i, (entry, phase)) in self.setup_systems.iter().enumerate() {
                if !self.setup_visible_in_stage(entry, si) {
                    continue;
                }
                let set_name = phase.phase_name().to_string();
                if let Some(last) = stage_setup_groups.last_mut() {
                    if last.0 == set_name {
                        last.1.push((i, entry));
                        continue;
                    }
                }
                stage_setup_groups.push((set_name, vec![(i, entry)]));
            }

            for (set_name, entries) in &stage_setup_groups {
                let setup_prefix = format!("{}setup", prefix);
                out.push_str(&format!(
                    "        subgraph cluster_{}_setup_{} {{\n",
                    set_name, si
                ));
                out.push_str(&format!("            label=\"Setup: {}\";\n", set_name));
                out.push_str("            style=filled; fillcolor=lightblue;\n");
                for &(i, entry) in entries {
                    out.push_str(&format!(
                        "            {} [label=\"{}\", {}];\n",
                        node_id(&setup_prefix, i),
                        make_label(entry, Some(si)),
                        node_style(entry, Some(si))
                    ));
                }
                out.push_str("        }\n");
            }

            // ── Update systems within this stage ─────────────────────────
            let mut stage_groups: Vec<(String, Vec<(usize, &StoredSystemEntry)>)> = Vec::new();
            for (i, (entry, phase)) in self.update_systems.iter().enumerate() {
                if !self.system_visible_in_stage(entry, si) {
                    continue;
                }
                let set_name = phase.phase_name().to_string();
                if let Some(last) = stage_groups.last_mut() {
                    if last.0 == set_name {
                        last.1.push((i, entry));
                        continue;
                    }
                }
                stage_groups.push((set_name, vec![(i, entry)]));
            }

            // Build first/last node ID maps for this stage
            let mut stage_first_id: HashMap<usize, String> = HashMap::new();
            let mut stage_last_id: HashMap<usize, String> = HashMap::new();

            for (set_name, entries) in &stage_groups {
                out.push_str(&format!(
                    "        subgraph cluster_{}_s{} {{\n",
                    set_name, si
                ));
                out.push_str(&format!("            label=\"{}\";\n", set_name));
                out.push_str("            style=filled; fillcolor=lightyellow;\n");
                for &(i, entry) in entries {
                    if let Some(info) = entry.system.group_info() {
                        Self::emit_group_subgraph(
                            out,
                            info,
                            entry,
                            i,
                            &prefix,
                            Some(si),
                            make_label,
                            "            ",
                        );
                        if !info.inner_systems.is_empty() {
                            stage_first_id.insert(i, format!("{}_{}_g0", prefix, i));
                            stage_last_id.insert(
                                i,
                                format!("{}_{}_g{}", prefix, i, info.inner_systems.len() - 1),
                            );
                        } else {
                            let nid = node_id(&prefix, i);
                            stage_first_id.insert(i, nid.clone());
                            stage_last_id.insert(i, nid);
                        }
                    } else {
                        let nid = node_id(&prefix, i);
                        out.push_str(&format!(
                            "            {} [label=\"{}\", {}];\n",
                            nid,
                            make_label(entry, Some(si)),
                            node_style(entry, Some(si))
                        ));
                        stage_first_id.insert(i, nid.clone());
                        stage_last_id.insert(i, nid);
                    }
                }
                out.push_str("        }\n");
            }

            out.push_str("    }\n\n");

            // Track first node for rank=same alignment
            let setup_prefix = format!("{}setup", prefix);
            let first_node_id = if let Some(first_sg) = stage_setup_groups.first() {
                node_id(&setup_prefix, first_sg.1.first().unwrap().0)
            } else if let Some(first_ug) = stage_groups.first() {
                stage_first_id[&first_ug.1.first().unwrap().0].clone()
            } else {
                continue; // empty stage
            };
            stage_first_nodes.push(first_node_id.clone());

            // ── Intra-stage edges ────────────────────────────────────────

            // Setup intra-group edges
            for (_set_name, entries) in &stage_setup_groups {
                for w in entries.windows(2) {
                    out.push_str(&format!(
                        "    {} -> {} [color=blue, style=bold];\n",
                        node_id(&setup_prefix, w[0].0),
                        node_id(&setup_prefix, w[1].0)
                    ));
                }
            }
            // Setup inter-group edges
            for g in 0..stage_setup_groups.len().saturating_sub(1) {
                let tail = node_id(&setup_prefix, stage_setup_groups[g].1.last().unwrap().0);
                let head = node_id(
                    &setup_prefix,
                    stage_setup_groups[g + 1].1.first().unwrap().0,
                );
                out.push_str(&format!(
                    "    {} -> {} [color=blue, style=bold];\n",
                    tail, head
                ));
            }

            // Setup → Run transition within this stage
            if let (Some(last_sg), Some(first_ug)) =
                (stage_setup_groups.last(), stage_groups.first())
            {
                let tail = node_id(&setup_prefix, last_sg.1.last().unwrap().0);
                let head = &stage_first_id[&first_ug.1.first().unwrap().0];
                out.push_str(&format!(
                    "    {} -> {} [color=blue, style=bold, label=\"start run\"];\n",
                    tail, head
                ));
            }

            // Update intra-group edges (using first/last IDs for groups)
            for (_set_name, entries) in &stage_groups {
                for w in entries.windows(2) {
                    out.push_str(&format!(
                        "    {} -> {} [color=blue, style=bold];\n",
                        stage_last_id[&w[0].0], stage_first_id[&w[1].0]
                    ));
                }
            }
            // Update inter-group edges
            for g in 0..stage_groups.len().saturating_sub(1) {
                let tail = &stage_last_id[&stage_groups[g].1.last().unwrap().0];
                let head = &stage_first_id[&stage_groups[g + 1].1.first().unwrap().0];
                out.push_str(&format!(
                    "    {} -> {} [color=blue, style=bold];\n",
                    tail, head
                ));
            }

            // Before/after constraint edges
            let mut label_to_node: HashMap<String, String> = HashMap::new();
            for (i, (entry, _)) in self.update_systems.iter().enumerate() {
                if !self.system_visible_in_stage(entry, si) {
                    continue;
                }
                if let Some(lbl) = &entry.label {
                    label_to_node.insert(
                        lbl.clone(),
                        stage_first_id
                            .get(&i)
                            .cloned()
                            .unwrap_or_else(|| node_id(&prefix, i)),
                    );
                }
            }
            for (i, (entry, _)) in self.update_systems.iter().enumerate() {
                if !self.system_visible_in_stage(entry, si) {
                    continue;
                }
                let from = stage_first_id
                    .get(&i)
                    .cloned()
                    .unwrap_or_else(|| node_id(&prefix, i));
                for b in &entry.befores {
                    if let Some(target) = label_to_node.get(b) {
                        out.push_str(&format!(
                            "    {} -> {} [color=red, style=dashed, label=\"before\"];\n",
                            from, target
                        ));
                    }
                }
                for a in &entry.afters {
                    if let Some(source) = label_to_node.get(a) {
                        out.push_str(&format!(
                            "    {} -> {} [color=red, style=dashed, label=\"after\"];\n",
                            source, from
                        ));
                    }
                }
            }
            for (i, (entry, _)) in self.update_systems.iter().enumerate() {
                if !self.system_visible_in_stage(entry, si) {
                    continue;
                }
                let from = stage_first_id
                    .get(&i)
                    .cloned()
                    .unwrap_or_else(|| node_id(&prefix, i));
                for req in &entry.requires {
                    if let Some(target) = label_to_node.get(req) {
                        out.push_str(&format!(
                            "    {} -> {} [color=purple, style=dotted, label=\"requires\", constraint=false];\n",
                            from, target
                        ));
                    }
                }
            }

            // Run loop: last update → first update
            if let (Some(first_ug), Some(last_ug)) = (stage_groups.first(), stage_groups.last()) {
                let first_run = &stage_first_id[&first_ug.1.first().unwrap().0];
                let last_run = &stage_last_id[&last_ug.1.last().unwrap().0];

                if stage_groups.len() >= 2 || first_ug.1.len() >= 2 {
                    out.push_str(&format!(
                        "    {} -> {} [color=green, style=bold, label=\"run loop\", constraint=false];\n",
                        last_run, first_run
                    ));
                }
            }
        }

        // Inter-stage "next stage" edges (constraint=false so they go sideways)
        // We connect from the last run node of stage N to the first setup node of stage N+1.
        for si in 0..self.stage_names.len().saturating_sub(1) {
            let this_prefix = format!("s{}", si);
            let next_prefix = format!("s{}", si + 1);
            let next_setup_prefix = format!("{}setup", next_prefix);

            // Find last update node of current stage (group-aware)
            let last_node = self
                .update_systems
                .iter()
                .enumerate()
                .rev()
                .find(|(_, (entry, _))| self.system_visible_in_stage(entry, si))
                .map(|(i, (entry, _))| {
                    if let Some(info) = entry.system.group_info() {
                        if !info.inner_systems.is_empty() {
                            return format!(
                                "{}_{}_g{}",
                                this_prefix,
                                i,
                                info.inner_systems.len() - 1
                            );
                        }
                    }
                    node_id(&this_prefix, i)
                });

            // Find first node of next stage (setup first, then update; group-aware)
            let first_node = self
                .setup_systems
                .iter()
                .enumerate()
                .find(|(_, (entry, _))| self.setup_visible_in_stage(entry, si + 1))
                .map(|(i, _)| node_id(&next_setup_prefix, i))
                .or_else(|| {
                    self.update_systems
                        .iter()
                        .enumerate()
                        .find(|(_, (entry, _))| self.system_visible_in_stage(entry, si + 1))
                        .map(|(i, (entry, _))| {
                            if let Some(info) = entry.system.group_info() {
                                if !info.inner_systems.is_empty() {
                                    return format!("{}_{}_g0", next_prefix, i);
                                }
                            }
                            node_id(&next_prefix, i)
                        })
                });

            if let (Some(from), Some(to)) = (last_node, first_node) {
                out.push_str(&format!(
                    "    {} -> {} [color=darkgreen, style=bold, penwidth=3, label=\"next stage\", constraint=false];\n",
                    from, to
                ));
            }
        }

        // Force stages side-by-side using rank=same on first nodes
        if stage_first_nodes.len() >= 2 {
            out.push_str(&format!(
                "    {{ rank=same; {} }}\n\n",
                stage_first_nodes.join("; ")
            ));
        }
    }
}

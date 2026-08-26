use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io;
use std::path::Path;

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use toml::Value;

const ROOT_BUILD: &str = "SBuild.toml";
const CONFIG: &str = ".config";
const CARGO_CONFIG: &str = ".config.cargo.toml";

#[derive(Clone, Debug)]
struct Module {
    id: String,
    name: String,
    description: Option<String>,
    options: Vec<OptionDef>,
}

#[derive(Clone, Debug)]
struct OptionDef {
    id: String,
    name: String,
    default: bool,
    feature: Option<String>,
    help: Option<String>,
}

#[derive(Clone, Debug)]
struct Entry {
    module_index: usize,
    option_index: usize,
}

type ConfigValues = BTreeMap<String, BTreeMap<String, bool>>;

fn main() {
    if let Err(error) = run() {
        eprintln!("ratconf: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let command = env::args()
        .nth(1)
        .unwrap_or_else(|| "menuconfig".to_string());
    let root = env::current_dir()?;
    let modules = discover_modules(&root)?;

    match command.as_str() {
        "menuconfig" => menuconfig(&root, &modules)?,
        "defconfig" => write_named_config(&root, &modules, "configs/defconfig.toml")?,
        "tinyconfig" => write_named_config(&root, &modules, "configs/tinyconfig.toml")?,
        "olddefconfig" => olddefconfig(&root, &modules)?,
        "savedefconfig" => savedefconfig(&root, &modules)?,
        "features" => print_features(&root, &modules)?,
        "help" | "--help" | "-h" => print_help(),
        other => return Err(format!("unknown command `{other}`").into()),
    }

    Ok(())
}

fn print_help() {
    println!("ratconf commands:");
    println!("  menuconfig     open the TUI");
    println!("  defconfig      write default config");
    println!("  tinyconfig     write tiny config");
    println!("  olddefconfig   preserve existing values and fill new defaults");
    println!("  savedefconfig  write only non-default values to defconfig.toml");
    println!("  features       print selected Cargo features as a comma list");
}

fn discover_modules(root: &Path) -> Result<Vec<Module>, Box<dyn std::error::Error>> {
    let root_build = root.join(ROOT_BUILD);
    let value = read_toml(&root_build)?;
    let roots = array_of_strings(&value, &["config", "roots"]);
    let mut modules = Vec::new();

    for config_root in roots {
        let path = root.join(config_root);
        discover_from_build(&path, &mut modules)?;
    }

    Ok(modules)
}

fn discover_from_build(
    build_path: &Path,
    modules: &mut Vec<Module>,
) -> Result<(), Box<dyn std::error::Error>> {
    let value = read_toml(build_path)?;
    let base = build_path.parent().unwrap_or_else(|| Path::new("."));

    for file in array_of_strings(&value, &["config", "files"]) {
        modules.push(read_sconfig(&base.join(file))?);
    }

    for child in array_of_strings(&value, &["config", "children"]) {
        let path = base.join(child);
        if path.file_name().and_then(|name| name.to_str()) == Some(ROOT_BUILD) {
            discover_from_build(&path, modules)?;
        } else {
            modules.push(read_sconfig(&path)?);
        }
    }

    Ok(())
}

fn read_sconfig(path: &Path) -> Result<Module, Box<dyn std::error::Error>> {
    let value = read_toml(path)?;
    let module = value
        .get("module")
        .and_then(Value::as_table)
        .ok_or_else(|| format!("{} has no [module] table", path.display()))?;

    let id = required_string(module.get("id"), path, "module.id")?;
    let name = required_string(module.get("name"), path, "module.name")?;
    let description = module
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut options = Vec::new();
    let option_values = value
        .get("option")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{} has no [[option]] entries", path.display()))?;

    for option in option_values {
        let table = option
            .as_table()
            .ok_or_else(|| format!("{} has a non-table option", path.display()))?;
        let kind = required_string(table.get("type"), path, "option.type")?;
        if kind != "bool" {
            return Err(format!(
                "{} option type `{kind}` is not supported yet",
                path.display()
            )
            .into());
        }

        options.push(OptionDef {
            id: required_string(table.get("id"), path, "option.id")?,
            name: required_string(table.get("name"), path, "option.name")?,
            default: table
                .get("default")
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("{} option.default must be bool", path.display()))?,
            feature: table
                .get("feature")
                .and_then(Value::as_str)
                .map(str::to_string),
            help: table
                .get("help")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }

    Ok(Module {
        id,
        name,
        description,
        options,
    })
}

fn required_string(value: Option<&Value>, path: &Path, name: &str) -> Result<String, String> {
    value
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{} {name} must be a string", path.display()))
}

fn read_toml(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)?;
    Ok(source.parse::<Value>()?)
}

fn array_of_strings(value: &Value, path: &[&str]) -> Vec<String> {
    let Some(array) = lookup(value, path).and_then(Value::as_array) else {
        return Vec::new();
    };

    array
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn lookup<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn default_values(modules: &[Module]) -> ConfigValues {
    let mut values = ConfigValues::new();
    for module in modules {
        let options = values.entry(module.id.clone()).or_default();
        for option in &module.options {
            options.insert(option.id.clone(), option.default);
        }
    }
    values
}

fn read_config(
    path: &Path,
    modules: &[Module],
) -> Result<ConfigValues, Box<dyn std::error::Error>> {
    let mut values = default_values(modules);
    if !path.exists() {
        return Ok(values);
    }

    let parsed = read_toml(path)?;
    for module in modules {
        for option in &module.options {
            if let Some(value) = lookup_dotted(&parsed, &module.id)
                .and_then(|table| table.get(&option.id))
                .and_then(Value::as_bool)
            {
                values
                    .entry(module.id.clone())
                    .or_default()
                    .insert(option.id.clone(), value);
            }
        }
    }

    Ok(values)
}

fn lookup_dotted<'a>(value: &'a Value, dotted: &str) -> Option<&'a toml::map::Map<String, Value>> {
    let mut current = value;
    for segment in dotted.split('.') {
        current = current.get(segment)?;
    }
    current.as_table()
}

fn write_named_config(
    root: &Path,
    modules: &[Module],
    config_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let values = read_config(&root.join(config_path), modules)?;
    write_outputs(root, modules, &values)
}

fn olddefconfig(root: &Path, modules: &[Module]) -> Result<(), Box<dyn std::error::Error>> {
    let values = read_config(&root.join(CONFIG), modules)?;
    write_outputs(root, modules, &values)
}

fn savedefconfig(root: &Path, modules: &[Module]) -> Result<(), Box<dyn std::error::Error>> {
    let values = read_config(&root.join(CONFIG), modules)?;
    let defaults = default_values(modules);
    let mut output = String::new();

    for module in modules {
        let mut lines = Vec::new();
        for option in &module.options {
            let value = value_for(&values, &module.id, &option.id, option.default);
            let default = value_for(&defaults, &module.id, &option.id, option.default);
            if value != default {
                lines.push(format!("{} = {}\n", option.id, value));
            }
        }

        if !lines.is_empty() {
            output.push_str(&format!("[{}]\n", module.id));
            for line in lines {
                output.push_str(&line);
            }
            output.push('\n');
        }
    }

    fs::write(root.join("defconfig.toml"), output)?;
    Ok(())
}

fn write_outputs(
    root: &Path,
    modules: &[Module],
    values: &ConfigValues,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(root.join(CONFIG), render_config(modules, values))?;
    fs::write(
        root.join(CARGO_CONFIG),
        render_cargo_config(modules, values),
    )?;
    Ok(())
}

fn render_config(modules: &[Module], values: &ConfigValues) -> String {
    let mut output = String::new();
    for module in modules {
        output.push_str(&format!("[{}]\n", module.id));
        for option in &module.options {
            let value = value_for(values, &module.id, &option.id, option.default);
            output.push_str(&format!("{} = {}\n", option.id, value));
        }
        output.push('\n');
    }
    output
}

fn render_cargo_config(modules: &[Module], values: &ConfigValues) -> String {
    let mut features = selected_features(modules, values)
        .into_iter()
        .collect::<Vec<_>>();
    features.sort();

    let mut output = String::from("[features]\nkernel = [\n");
    for feature in features {
        output.push_str(&format!("    \"{}\",\n", feature));
    }
    output.push_str("]\n");
    output
}

fn selected_features(modules: &[Module], values: &ConfigValues) -> BTreeSet<String> {
    let mut features = BTreeSet::new();
    for module in modules {
        for option in &module.options {
            let enabled = value_for(values, &module.id, &option.id, option.default);
            if enabled && let Some(feature) = &option.feature {
                features.insert(feature.clone());
            }
        }
    }
    features
}

fn value_for(values: &ConfigValues, module: &str, option: &str, default: bool) -> bool {
    values
        .get(module)
        .and_then(|options| options.get(option))
        .copied()
        .unwrap_or(default)
}

fn print_features(root: &Path, modules: &[Module]) -> Result<(), Box<dyn std::error::Error>> {
    let values = read_config(&root.join(CONFIG), modules)?;
    let features = selected_features(modules, &values)
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    println!("{features}");
    Ok(())
}

fn menuconfig(root: &Path, modules: &[Module]) -> Result<(), Box<dyn std::error::Error>> {
    let mut values = read_config(&root.join(CONFIG), modules)?;
    let entries = entries_for(modules);
    if entries.is_empty() {
        return Err("no config options discovered".into());
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_tui(&mut terminal, modules, &entries, &mut values);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result?;
    write_outputs(root, modules, &values)?;
    Ok(())
}

fn entries_for(modules: &[Module]) -> Vec<Entry> {
    let mut entries = Vec::new();
    for (module_index, module) in modules.iter().enumerate() {
        for (option_index, _) in module.options.iter().enumerate() {
            entries.push(Entry {
                module_index,
                option_index,
            });
        }
    }
    entries
}

fn run_tui(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    modules: &[Module],
    entries: &[Entry],
    values: &mut ConfigValues,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut selected = 0usize;
    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(5)])
                .split(area);

            let items = entries.iter().enumerate().map(|(index, entry)| {
                let module = &modules[entry.module_index];
                let option = &module.options[entry.option_index];
                let value = value_for(values, &module.id, &option.id, option.default);
                let checkbox = if value { "[*]" } else { "[ ]" };
                let marker = if index == selected { "> " } else { "  " };
                ListItem::new(Line::from(vec![
                    Span::raw(marker),
                    Span::styled(module.name.as_str(), Style::default().fg(Color::Cyan)),
                    Span::raw(" / "),
                    Span::raw(checkbox),
                    Span::raw(" "),
                    Span::raw(option.name.as_str()),
                ]))
            });

            let mut state = ListState::default();
            state.select(Some(selected));
            let list = List::new(items)
                .block(
                    Block::default()
                        .title("SaeOS ratconf")
                        .borders(Borders::ALL),
                )
                .highlight_style(Style::default().add_modifier(Modifier::BOLD));
            frame.render_stateful_widget(list, chunks[0], &mut state);

            let entry = &entries[selected];
            let module = &modules[entry.module_index];
            let option = &module.options[entry.option_index];
            let help = option
                .help
                .as_deref()
                .or(module.description.as_deref())
                .unwrap_or("No help available.");
            let footer = Paragraph::new(format!(
                "{}.{} | space toggles | s saves | q quits\n{}",
                module.id, option.id, help
            ))
            .block(Block::default().title("Help").borders(Borders::ALL));
            frame.render_widget(footer, chunks[1]);
        })?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('s') => break,
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(entries.len() - 1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Char(' ') | KeyCode::Enter => {
                    let entry = &entries[selected];
                    let module = &modules[entry.module_index];
                    let option = &module.options[entry.option_index];
                    let current = value_for(values, &module.id, &option.id, option.default);
                    values
                        .entry(module.id.clone())
                        .or_default()
                        .insert(option.id.clone(), !current);
                }
                _ => {}
            }
        }
    }

    Ok(())
}

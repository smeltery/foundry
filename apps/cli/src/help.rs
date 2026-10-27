use clap::CommandFactory;
use serde_json::json;

use crate::{CommandError, Response, args::Cli};

pub fn render(path: &[String]) -> Result<Response, CommandError> {
    let mut command = Cli::command();
    command.build();
    let text = if path == ["all"] {
        let mut sections = Vec::new();
        collect(&command, "f5y", &mut sections);
        sections.join("\n\n")
    } else {
        let mut name = String::from("f5y");
        for part in path {
            let child = command
                .get_subcommands()
                .find(|child| {
                    child.get_name() == part || child.get_all_aliases().any(|alias| alias == part)
                })
                .cloned()
                .ok_or_else(|| {
                    CommandError::new("usage", format!("Unknown help command: {name} {part}"))
                })?;
            command = child;
            name.push(' ');
            name.push_str(command.get_name());
        }
        command.bin_name(name).render_long_help().to_string()
    };
    Ok(Response {
        json: json!({"help": text}),
        text,
        exit_code: 0,
    })
}

fn collect(command: &clap::Command, name: &str, sections: &mut Vec<String>) {
    sections.push(format!(
        "{name}\n{}",
        command.clone().bin_name(name).render_long_help()
    ));
    for child in command.get_subcommands() {
        collect(child, &format!("{name} {}", child.get_name()), sections);
    }
}

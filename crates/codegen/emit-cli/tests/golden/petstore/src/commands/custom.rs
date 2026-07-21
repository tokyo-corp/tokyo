pub fn augment(command: clap::Command) -> clap::Command {
    command.subcommand(
        clap::Command::new("hello")
            .about("Print a custom greeting")
            .arg(clap::Arg::new("name").long("name").default_value("world")),
    )
}

pub fn dispatch(
    matches: &clap::ArgMatches,
    context: &crate::cli::CommandContext<'_>,
) -> Result<bool, crate::error::ClientError> {
    let Some(("hello", matches)) = matches.subcommand() else {
        return Ok(false);
    };
    crate::output::print_serialized_response(
        &serde_json::json!({
            "greeting": format!(
                "Hello, {}!",
                matches
                    .get_one::<String>("name")
                    .expect("name has a default value"),
            ),
            "profile": context.profile,
        }),
        context.output,
    );
    Ok(true)
}

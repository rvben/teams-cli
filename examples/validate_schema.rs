use teams_cli::schema;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let official: serde_json::Value = reqwest::Client::builder()
        .user_agent("teams-cli-validation")
        .build()?
        .get("https://clispec.dev/schema/v0.3.json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let validator = jsonschema::validator_for(&official)?;
    let document = schema::generate(None);
    if let Err(error) = validator.validate(&document) {
        return Err(
            std::io::Error::other(format!("{} at {}", error, error.instance_path())).into(),
        );
    }
    println!("CLI Spec v0.3 schema validation: pass");
    Ok(())
}

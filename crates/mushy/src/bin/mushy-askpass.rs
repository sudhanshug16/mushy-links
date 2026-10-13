use std::io::{Read, Write};
fn main() {
    let result = (|| -> anyhow::Result<()> {
        let host = std::env::var("MUSHY_AUTH_HOST")?;
        let argument = std::env::args().nth(1).unwrap_or_default();
        let entry = keyring::Entry::new("mushy-links", &host)?;
        if argument == "--store" {
            let mut password = String::new();
            std::io::stdin().take(65537).read_to_string(&mut password)?;
            anyhow::ensure!(password.len() <= 65536, "password too long");
            entry.set_password(&password)?;
            return Ok(());
        }
        if argument == "--forget" {
            entry.delete_credential()?;
            return Ok(());
        }
        let prompt = argument.to_ascii_lowercase();
        anyhow::ensure!(
            prompt.contains("password") && !prompt.contains("passphrase"),
            "unsupported authentication prompt"
        );
        let password = entry.get_password()?;
        std::io::stdout().write_all(password.as_bytes())?;
        std::io::stdout().write_all(b"\n")?;
        Ok(())
    })();
    if result.is_err() {
        std::process::exit(1);
    }
}

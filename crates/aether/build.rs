fn main() -> Result<(), Box<dyn std::error::Error>> {
    topcoat::tailwind::BuildConfig::new().input("styles.css").render()?;
    Ok(())
}

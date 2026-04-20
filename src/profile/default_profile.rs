use super::model::Profile;

pub fn built_in_default() -> Profile {
    Profile::new(
        "default",
        "You are a helpful, thoughtful assistant.\n",
        "Be honest. Do not make things up.\nBe concise unless the user asks for depth.\n",
        "",
        vec![],
        Default::default(),
    )
}

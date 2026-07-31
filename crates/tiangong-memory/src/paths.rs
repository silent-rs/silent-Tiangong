use std::path::PathBuf;

pub(crate) fn memory_data_dir() -> PathBuf {
    if let Some(path) =
        std::env::var_os("TIANGONG_PLUGIN_DATA_DIR").filter(|value| !value.is_empty())
    {
        return PathBuf::from(path);
    }

    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
        .join("memory")
}

fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

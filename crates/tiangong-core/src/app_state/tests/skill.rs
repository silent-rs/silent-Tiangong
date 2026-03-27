use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use super::common::with_isolated_state;
use crate::skill::build_skill_hints;

#[test]
fn install_skill_saves_into_user_skills_dir_and_recognizable() -> Result<()> {
    with_isolated_state("tiangong-skill-install", |paths, state| {
        let nonce = scru128::new().to_string();
        let source_dir = paths.workspace.join("demo-skill-src");
        fs::create_dir_all(&source_dir)?;

        let skill_id = format!("demo-skill-{nonce}");
        let skill_md = source_dir.join("SKILL.md");
        let skill_toml = source_dir.join("skill.toml");
        fs::write(
            &skill_md,
            "# Demo Skill\n用于测试安装行为是否写入 ~/.tiangong/skills。\n",
        )?;
        fs::write(
            &skill_toml,
            format!(
                "id = \"{skill_id}\"\nname = \"Demo Skill\"\nversion = \"0.1.0\"\nentry = \"SKILL.md\"\n\n[source]\ntype = \"local\"\nvalue = \"{}\"\n\n[requires]\nmcp = []\n\n[permissions]\nfs_read = [\"./**\"]\nfs_write = []\ncmd_exec = []\nnet = []\n",
                source_dir.display()
            ),
        )?;

        let install_message =
            state.install_local_skill(source_dir.to_str().unwrap_or_default(), true)?;
        assert!(install_message.contains("skill 已安装"));

        let installed = state
            .installed_skills()
            .iter()
            .find(|skill| skill.id == skill_id)
            .cloned()
            .expect("安装后应可在配置中找到 skill");
        let installed_path = PathBuf::from(installed.source.value.clone());
        let installed_skill_root = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join("installed")
            .join(&skill_id);
        let expected_prefix = paths
            .fake_home
            .join(".tiangong")
            .join("skills")
            .join("installed")
            .join(&skill_id)
            .join("0.1.0");
        assert!(installed_path.starts_with(&expected_prefix));
        assert!(installed_path.join("SKILL.md").exists());
        assert!(paths.fake_home.join(".tiangong").join("app.json").exists());
        assert!(
            paths
                .fake_home
                .join(".tiangong")
                .join("skills.json")
                .exists()
        );
        assert!(paths.fake_home.join(".tiangong").join("mcp.json").exists());
        assert!(
            paths
                .fake_home
                .join(".tiangong")
                .join("skills")
                .join("skills-lock.json")
                .exists()
        );

        let hints = build_skill_hints("demo skill", &state.store.agent.agent_config.skills);
        assert!(hints.iter().any(|item| item.contains(&skill_id)));

        state.remove_skill(&skill_id)?;
        assert!(!installed_path.exists());
        assert!(!installed_skill_root.exists());
        Ok(())
    })
}

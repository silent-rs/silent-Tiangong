//! 密钥/Token 的环境变量解析辅助。
//!
//! 配置中的 api_key 支持 `${VAR}` 模板引用环境变量。
//! 结构校验（config validate）只需字段非空；而诊断/测试类命令
//! （doctor / model test / memory test）需要确保 `${VAR}` 能解析到真实值，
//! 否则纯服务端最常见的"环境变量未设置"问题无法被发现。

/// 判断 `${VAR}` 引用的环境变量是否可解析（用于诊断报告）。
///
/// 返回 `(是否可解析, 解析失败时的变量名)`。
///
/// - 非 `${...}` 形式：`(true, None)`。
/// - `${VAR}` 且变量存在：`(true, None)`。
/// - `${VAR}` 且变量不存在：`(false, Some("VAR"))`。
pub fn env_secret_resolvable(value: &str) -> (bool, Option<String>) {
    let trimmed = value.trim();
    if let Some(inner) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        let var_name = inner.trim();
        if var_name.is_empty() {
            return (true, None);
        }
        if std::env::var(var_name).is_ok() {
            (true, None)
        } else {
            (false, Some(var_name.to_string()))
        }
    } else {
        (true, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_value_is_resolvable() {
        assert_eq!(env_secret_resolvable("sk-plain-key"), (true, None));
    }

    #[test]
    fn env_var_resolvable_when_present() {
        // safety: 测试串行执行，仅设置临时变量后立即清理
        unsafe {
            std::env::set_var("TIANGONG_TEST_SECRET_KEY", "resolved-value");
        }
        let result = env_secret_resolvable("${TIANGONG_TEST_SECRET_KEY}");
        // safety: 同上，清理测试变量
        unsafe {
            std::env::remove_var("TIANGONG_TEST_SECRET_KEY");
        }
        assert_eq!(result, (true, None));
    }

    #[test]
    fn env_var_not_resolvable_when_missing() {
        let result = env_secret_resolvable("${TIANGONG_DEFINITELY_MISSING_VAR_XYZ_123}");
        assert_eq!(
            result,
            (
                false,
                Some("TIANGONG_DEFINITELY_MISSING_VAR_XYZ_123".to_string())
            )
        );
    }

    #[test]
    fn empty_env_name_is_resolvable() {
        assert_eq!(env_secret_resolvable("${}"), (true, None));
    }
}

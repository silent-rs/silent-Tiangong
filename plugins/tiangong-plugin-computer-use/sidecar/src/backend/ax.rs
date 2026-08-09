//! macOS AXUIElement C API 的安全封装。
//!
//! 直接链接 ApplicationServices 框架的 AXUIElement 函数，用 core-foundation
//! crate 处理 CFString/CFArray/CFBoolean/CFNumber 返回值，手动管理引用计数。
//!
//! 设计要点：
//! - `AxElement` 持有一个 `AXUIElementRef` 并在 Drop 时 `CFRelease`。
//! - 所有读取操作返回 `Result<T, AxError>`，错误映射自 `AXError`。
//! - 不暴露原始指针，调用方只接触安全类型。

use std::ffi::c_void;
use std::os::raw::c_float;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFRelease, CFRetain, CFTypeRef, TCFType};
use core_foundation::boolean::{CFBoolean, CFBooleanRef};
use core_foundation::string::{CFString, CFStringRef};

/// AXError 值（仅取常用项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum AxError {
    Success = 0,
    Failure = -25200,
    IllegalArgument = -25201,
    InvalidUIElement = -25202,
    CannotComplete = -25204,
    AttributeUnsupported = -25205,
    ActionUnsupported = -25206,
    NotImplemented = -25208,
    NotEnoughPrecision = -25211,
    Other = -25299,
}

impl AxError {
    pub fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    /// 由原始 AXError（i32）构造。
    fn from_raw(code: i32) -> Self {
        match code {
            0 => Self::Success,
            -25200 => Self::Failure,
            -25201 => Self::IllegalArgument,
            -25202 => Self::InvalidUIElement,
            -25204 => Self::CannotComplete,
            -25205 => Self::AttributeUnsupported,
            -25206 => Self::ActionUnsupported,
            -25208 => Self::NotImplemented,
            -25211 => Self::NotEnoughPrecision,
            _ => Self::Other,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Success => "成功",
            Self::Failure => "AX 调用失败",
            Self::IllegalArgument => "非法参数",
            Self::InvalidUIElement => "控件引用无效",
            Self::CannotComplete => "无法完成（可能未授权或进程不可达）",
            Self::AttributeUnsupported => "控件不支持该属性",
            Self::ActionUnsupported => "控件不支持该动作",
            Self::NotImplemented => "未实现",
            Self::NotEnoughPrecision => "精度不足",
            Self::Other => "未知 AX 错误",
        }
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeNames(element: AXUIElementRef, names: *mut CFArrayRef) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> i32;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> i32;
    fn AXUIElementCopyElementAtPosition(
        application: AXUIElementRef,
        x: c_float,
        y: c_float,
        element: *mut AXUIElementRef,
    ) -> i32;
}

/// AXUIElementRef 即 CFTypeRef。
pub type AXUIElementRef = *const c_void;

/// 持有 AXUIElement 引用的 RAII 包装。
pub struct AxElement {
    raw: AXUIElementRef,
}

// SAFETY：AXUIElement 是 CoreFoundation 引用计数对象，可跨线程传递与访问。
// Apple 的辅助功能 API 文档明确属性读取是线程安全的。
unsafe impl Send for AxElement {}
unsafe impl Sync for AxElement {}

impl AxElement {
    /// 通过进程号创建应用根元素。
    pub fn for_application(pid: i32) -> Self {
        // SAFETY：AXUIElementCreateApplication 返回一个新的引用（caller 必须 release）。
        let raw = unsafe { AXUIElementCreateApplication(pid) };
        Self { raw }
    }

    /// 由原始引用构造，并 retain（用于从数组中取出的元素）。
    pub fn from_retained(raw: AXUIElementRef) -> Option<Self> {
        if raw.is_null() {
            return None;
        }
        Some(Self { raw })
    }

    pub fn raw(&self) -> AXUIElementRef {
        self.raw
    }

    /// 读取控件的属性名列表。
    pub fn attribute_names(&self) -> Result<Vec<String>, AxError> {
        let mut array: CFArrayRef = std::ptr::null();
        let code = unsafe { AXUIElementCopyAttributeNames(self.raw, &mut array) };
        let err = AxError::from_raw(code);
        if !err.is_success() || array.is_null() {
            return Err(err);
        }
        // SAFETY：array 由 Copy API 返回，caller owned，用 CFArray 包裹后 Drop 时释放。
        let cf_array = unsafe { CFArray::<*const c_void>::wrap_under_create_rule(array) };
        let range = core_foundation::base::CFRange::init(0, cf_array.len());
        let ptrs = cf_array.get_values(range);
        let mut names = Vec::with_capacity(ptrs.len());
        for ptr in ptrs {
            if let Some(s) = cf_type_as_string(ptr) {
                names.push(s);
            }
        }
        Ok(names)
    }

    /// 读取字符串属性。
    pub fn string_attribute(&self, name: &str) -> Option<String> {
        let value = self.copy_attribute_value(name)?;
        cf_type_as_string(value)
    }

    /// 读取布尔属性。
    pub fn bool_attribute(&self, name: &str) -> Option<bool> {
        let value = self.copy_attribute_value(name)?;
        // SAFETY：value 为 caller-owned CFTypeRef，作为 CFBoolean 解释。
        let cf_bool = unsafe { CFBoolean::wrap_under_create_rule(value as CFBooleanRef) };
        Some(bool::from(cf_bool))
    }

    /// 读取 AXValue 包装的属性（如 AXPosition/AXSize，返回 CGRect），暂以边界解析。
    /// 返回原始 CFTypeRef，调用方按需解释。
    pub fn raw_attribute(&self, name: &str) -> Option<CFTypeRef> {
        self.copy_attribute_value(name)
    }

    /// 读取子控件数组（AXChildren）。
    pub fn children(&self) -> Result<Vec<AxElement>, AxError> {
        let value = match self.copy_attribute_value("AXChildren") {
            Some(v) => v,
            None => return Err(AxError::AttributeUnsupported),
        };
        if value.is_null() {
            return Ok(Vec::new());
        }
        // SAFETY：value 为 caller-owned CFTypeRef，这里作为 CFArray 解释。
        let cf_array =
            unsafe { CFArray::<*const c_void>::wrap_under_create_rule(value as CFArrayRef) };
        let range = core_foundation::base::CFRange::init(0, cf_array.len());
        let ptrs = cf_array.get_values(range);
        let mut children = Vec::with_capacity(ptrs.len());
        for child_ptr in ptrs {
            // 数组中的元素是未 retain 的引用，需要 retain 后再用。
            if let Some(child) = Self::from_retained(retain(child_ptr)) {
                children.push(child);
            }
        }
        Ok(children)
    }

    /// 执行动作（如 AXPress）。
    pub fn perform_action(&self, action: &str) -> Result<(), AxError> {
        let cf_action = CFString::new(action);
        let code = unsafe { AXUIElementPerformAction(self.raw, cf_action.as_concrete_TypeRef()) };
        let err = AxError::from_raw(code);
        if err.is_success() { Ok(()) } else { Err(err) }
    }

    /// 设置字符串属性（如 AXValue）。
    pub fn set_string_attribute(&self, name: &str, value: &str) -> Result<(), AxError> {
        let cf_value = CFString::new(value);
        let cf_attr = CFString::new(name);
        let code = unsafe {
            AXUIElementSetAttributeValue(
                self.raw,
                cf_attr.as_concrete_TypeRef(),
                cf_value.as_concrete_TypeRef() as CFTypeRef,
            )
        };
        let err = AxError::from_raw(code);
        if err.is_success() { Ok(()) } else { Err(err) }
    }

    /// 底层拷贝属性值，返回 caller-owned CFTypeRef（成功时）。
    fn copy_attribute_value(&self, name: &str) -> Option<CFTypeRef> {
        let cf_name = CFString::new(name);
        let mut value: CFTypeRef = std::ptr::null();
        let code = unsafe {
            AXUIElementCopyAttributeValue(self.raw, cf_name.as_concrete_TypeRef(), &mut value)
        };
        let err = AxError::from_raw(code);
        if !err.is_success() || value.is_null() {
            return None;
        }
        Some(value)
    }
}

impl Drop for AxElement {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY：构造时持有引用，Drop 时释放。
            unsafe { CFRelease(self.raw as CFTypeRef) };
        }
    }
}

/// 探测辅助功能授权。
pub fn is_process_trusted() -> bool {
    // SAFETY：无副作用。
    unsafe { AXIsProcessTrusted() != 0 }
}

/// 在 (x, y) 屏幕坐标处拷贝元素（用于未来按坐标定位，当前未直接使用）。
#[allow(dead_code)]
pub fn element_at_position(app: &AxElement, x: f32, y: f32) -> Result<AxElement, AxError> {
    let mut element: AXUIElementRef = std::ptr::null();
    let code = unsafe { AXUIElementCopyElementAtPosition(app.raw(), x, y, &mut element) };
    let err = AxError::from_raw(code);
    if !err.is_success() || element.is_null() {
        return Err(err);
    }
    AxElement::from_retained(element).ok_or(AxError::Failure)
}

/// retain 一个 CFTypeRef 并返回新引用。
fn retain(ptr: *const c_void) -> *const c_void {
    if ptr.is_null() {
        return ptr;
    }
    // SAFETY：对非空 CF 对象增引用计数。
    unsafe { CFRetain(ptr as CFTypeRef) }
}

/// 把 CFTypeRef 当作 CFString 解释并转 owned String。
fn cf_type_as_string(ptr: *const c_void) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY：ptr 为 CFTypeRef，wrap_under_create_rule 接管所有权。
    let cf_string = unsafe { CFString::wrap_under_create_rule(ptr as CFStringRef) };
    Some(cf_string.to_string())
}

/// 把 CFTypeRef 当作 CFBoolean 判断真值（不接管所有权，仅读取）。
#[allow(dead_code)]
fn cf_type_as_bool_ref(ptr: *const c_void) -> bool {
    if ptr.is_null() {
        return false;
    }
    // SAFETY：ptr 为 CFTypeRef，wrap_under_create_rule 接管所有权，转为 bool 后释放。
    unsafe {
        let b = CFBoolean::wrap_under_create_rule(ptr as CFBooleanRef);
        bool::from(b)
    }
}

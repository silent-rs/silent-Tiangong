//! 文档附件解析规则段（issue #149）
//!
//! 用户上传的 PDF/Office 文件**一律走本地脚本解析**，不依赖多模态模型能力。
//! 文件归档在 `~/.tiangong/media/files/`，解析产生的结果文件写入当前 workspace。
//! 本段把上述约定和具体解析方法嵌入 system prompt，由 agent 运行时自主执行。

/// 构建文档附件解析规则段。
pub fn attachment_rules_section() -> String {
    "## 文档附件解析规则\n\
     用户上传的 PDF / Word(.docx) / Excel(.xlsx) / PowerPoint(.pptx) 文件**统一用本地脚本解析**，不直接交给多模态模型。\n\
     \n\
     ### 文件位置\n\
     - 源文件已归档在 `~/.tiangong/media/files/`，用户消息的附件提示中给出 `path=<本地路径>`，可直接读取。\n\
     - **解析产生的结果文件（提取的文本/转换的格式/生成的报告等）必须写入当前 workspace 目录**，不要写到归档目录或系统其他位置。\n\
     \n\
     ### 解析方法（按格式选用）\n\
     - PDF：`python3` + pdfplumber（`pip install pdfplumber`）\n\
     - docx：`python3` + python-docx（`pip install python-docx`，`from docx import Document`）\n\
     - xlsx：`python3` + openpyxl（`pip install openpyxl`）\n\
     - pptx：`python3` + python-pptx（`pip install python-pptx`）\n\
     - Node 备选：pdf-parse / mammoth / exceljs / pptxtojson\n\
     \n\
     ### 依赖安装（隔离，不污染系统环境）\n\
     ```\n\
     pip install --target ~/.tiangong/parsers/python <package>\n\
     PYTHONPATH=~/.tiangong/parsers/python python3 -c \"...\"\n\
     npm install --prefix ~/.tiangong/parsers/node <package>\n\
     ```\n\
     \n\
     ### 输出规范\n\
     1. 解析结果转为 Markdown（保留标题层级、表格、列表结构）后引用。\n\
     2. 大文件按页/段分批处理，避免单次注入过多内容。\n\
     3. 解析大文件可能超过默认 30s 超时，用 `__tiangong_timeout=120000` 覆盖。\n\
     4. 解析失败必须如实告知用户，不得虚构结果。\n\
     5. 若产生中间/结果文件（如提取的 .md、转换的 .csv），写入 workspace 并告知用户路径。"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_contains_key_libraries() {
        let section = attachment_rules_section();
        assert!(section.contains("pdfplumber"));
        assert!(section.contains("python-docx"));
        assert!(section.contains("openpyxl"));
        assert!(section.contains("python-pptx"));
    }

    #[test]
    fn section_emphasizes_local_parsing_and_workspace_output() {
        let section = attachment_rules_section();
        assert!(section.contains("~/.tiangong/media/files/"));
        assert!(section.contains("workspace"));
        assert!(section.contains("~/.tiangong/parsers"));
    }
}

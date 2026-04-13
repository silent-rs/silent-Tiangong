#!/usr/bin/env python3
# /// script
# requires-python = ">=3.8"
# dependencies = ["requests>=2.28"]
# ///
"""
火山方舟（Volcengine Ark）视频生成脚本

使用 Seedance 模型通过火山方舟 API 生成视频。
API 参考：https://www.volcengine.com/docs/82379/1520757

运行方式：pipx run generate.py --prompt "视频描述"

环境变量（在 .env / .env.local 中配置）：
  ARK_API_KEY: 火山方舟 API Key
  ARK_BASE_URL: API 基础地址（默认 https://ark.cn-beijing.volces.com/api/v3）
  ARK_MODEL: 模型接入点 ID（如 ep-xxx 或模型名）
"""

import argparse
import json
import os
import sys
import time
import requests


def load_env():
    """从 .env 文件加载环境变量"""
    # 先从脚本所在目录加载，再从当前目录加载
    script_dir = os.path.dirname(os.path.abspath(__file__))
    env_files = [
        os.path.join(script_dir, ".env.local"),
        os.path.join(script_dir, ".env"),
        ".env.local",
        ".env",
    ]
    for env_file in env_files:
        if os.path.exists(env_file):
            with open(env_file) as f:
                for line in f:
                    line = line.strip()
                    if line and not line.startswith("#") and "=" in line:
                        key, _, value = line.partition("=")
                        key = key.strip()
                        value = value.strip().strip("'\"")
                        if key and key not in os.environ:
                            os.environ[key] = value


def submit_task(api_key: str, base_url: str, model: str, prompt: str) -> dict:
    """
    创建视频生成任务
    POST {base_url}/contents/generations/tasks
    """
    url = f"{base_url}/contents/generations/tasks"
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
    }
    body = {
        "model": model,
        "content": [
            {
                "type": "text",
                "text": prompt,
            }
        ],
    }

    resp = requests.post(url, headers=headers, json=body, timeout=60)
    if not resp.ok:
        detail = resp.text[:500] if resp.text else ""
        raise Exception(f"HTTP {resp.status_code}: {detail}")
    return resp.json()


def query_task(api_key: str, base_url: str, task_id: str) -> dict:
    """
    查询视频生成任务状态
    GET {base_url}/contents/generations/tasks/{task_id}
    """
    url = f"{base_url}/contents/generations/tasks/{task_id}"
    headers = {
        "Authorization": f"Bearer {api_key}",
    }
    resp = requests.get(url, headers=headers, timeout=15)
    if not resp.ok:
        raise Exception(f"查询失败 HTTP {resp.status_code}")
    return resp.json()


def extract_video_url(data: dict) -> str:
    """从任务响应中提取视频 URL"""
    # 直接字段
    for key in ["video_url", "url", "video"]:
        val = data.get(key, "")
        if isinstance(val, str) and val.startswith("http"):
            return val

    # content 字段
    content = data.get("content", {})
    if isinstance(content, dict):
        for key in ["video_url", "url"]:
            val = content.get(key, "")
            if isinstance(val, str) and val.startswith("http"):
                return val
    if isinstance(content, list):
        for item in content:
            if isinstance(item, dict):
                for key in ["video_url", "url"]:
                    val = item.get(key, "")
                    if isinstance(val, str) and val.startswith("http"):
                        return val

    # choices 字段（chat completions 兼容格式）
    choices = data.get("choices", [])
    if choices:
        msg = choices[0].get("message", {})
        c = msg.get("content", "")
        if isinstance(c, str) and c.strip().startswith("http"):
            return c.strip()

    return ""


def generate_video(prompt: str, model: str, max_wait: int = 300) -> dict:
    """完整流程：提交 → 轮询 → 返回结果"""
    api_key = os.environ.get("ARK_API_KEY", "")
    base_url = os.environ.get(
        "ARK_BASE_URL", "https://ark.cn-beijing.volces.com/api/v3"
    )
    base_url = base_url.rstrip("/")

    # 支持通过环境变量覆盖模型
    env_model = os.environ.get("ARK_MODEL", "")
    if env_model:
        model = env_model

    if not api_key:
        return {"ok": False, "error": "未配置 ARK_API_KEY，请在 .env.local 中设置"}

    # 提交任务
    try:
        result = submit_task(api_key, base_url, model, prompt)
    except Exception as e:
        return {"ok": False, "error": f"提交视频生成任务失败：{e}"}

    # 检查错误
    if "error" in result and result["error"]:
        err = result["error"]
        msg = err.get("message", str(err)) if isinstance(err, dict) else str(err)
        return {"ok": False, "error": f"API 返回错误：{msg}"}

    task_id = result.get("id", "")
    if not task_id:
        return {
            "ok": False,
            "error": "响应缺少 id 字段",
            "response": json.dumps(result, ensure_ascii=False)[:500],
        }

    # 检查是否直接返回了结果
    status = result.get("status", "")
    if status in ("succeeded", "completed"):
        video_url = extract_video_url(result)
        if video_url:
            return {"ok": True, "video_url": video_url, "task_id": task_id}

    # 轮询等待
    print(f"任务已提交 (id={task_id})，等待生成...", file=sys.stderr)
    start_time = time.time()

    while time.time() - start_time < max_wait:
        time.sleep(5)
        elapsed = int(time.time() - start_time)
        print(f"  轮询中... ({elapsed}s)", file=sys.stderr)

        try:
            status_data = query_task(api_key, base_url, task_id)
        except Exception as e:
            print(f"  查询失败：{e}，继续...", file=sys.stderr)
            continue

        status = status_data.get("status", "")

        if status in ("succeeded", "completed", "complete"):
            video_url = extract_video_url(status_data)
            if video_url:
                return {
                    "ok": True,
                    "video_url": video_url,
                    "task_id": task_id,
                    "duration_seconds": elapsed,
                }
            return {
                "ok": False,
                "error": "生成完成但未找到视频 URL",
                "task_id": task_id,
                "response": json.dumps(status_data, ensure_ascii=False)[:500],
            }

        if status in ("failed", "error"):
            error_info = status_data.get("error", {})
            msg = (
                error_info.get("message", str(error_info))
                if isinstance(error_info, dict)
                else str(error_info)
            )
            return {"ok": False, "error": f"视频生成失败：{msg}", "task_id": task_id}

        # pending / running / processing → 继续

    return {"ok": False, "error": f"视频生成超时（{max_wait}秒）", "task_id": task_id}


def download_video(url: str, output_path: str) -> str:
    """下载视频到本地"""
    resp = requests.get(url, stream=True, timeout=120)
    resp.raise_for_status()
    os.makedirs(os.path.dirname(os.path.abspath(output_path)), exist_ok=True)
    with open(output_path, "wb") as f:
        for chunk in resp.iter_content(chunk_size=8192):
            f.write(chunk)
    return os.path.abspath(output_path)


def main():
    parser = argparse.ArgumentParser(description="火山方舟视频生成")
    parser.add_argument("--prompt", required=True, help="视频内容描述")
    parser.add_argument(
        "--model", default="doubao-seedance-1-0-pro-fast-251015", help="模型名称或接入点 ID"
    )
    parser.add_argument("--max-wait", type=int, default=300, help="最大等待时间（秒）")
    parser.add_argument("--output", "-o", default="", help="下载视频到本地路径（可选）")
    args = parser.parse_args()

    load_env()
    result = generate_video(args.prompt, args.model, args.max_wait)

    # 成功且指定了 output 路径时自动下载
    if result.get("ok") and args.output and result.get("video_url"):
        try:
            local_path = download_video(result["video_url"], args.output)
            result["local_path"] = local_path
            print(f"视频已下载到：{local_path}", file=sys.stderr)
        except Exception as e:
            result["download_error"] = str(e)
            print(f"下载失败：{e}", file=sys.stderr)

    print(json.dumps(result, ensure_ascii=False, indent=2))

    if not result.get("ok"):
        sys.exit(1)


if __name__ == "__main__":
    main()

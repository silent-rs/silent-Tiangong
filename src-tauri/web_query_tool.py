#!/usr/bin/env python3
"""
网络查询工具 - 用于查询 GLM 模型硬件要求和 GPU 性能信息
"""

import sys
import json
import requests
from typing import Dict, List, Optional
from datetime import datetime


class WebQueryTool:
    def __init__(self):
        self.headers = {
            'User-Agent': 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36'
        }
    
    def search_glm_requirements(self) -> Dict:
        """查询 GLM 模型的硬件要求"""
        print("🔍 正在查询 GLM 5.1/GLM-4 模型硬件要求...")
        
        # 尝试从 GitHub API 查询 GLM 模型仓库信息
        results = {
            'source': 'GitHub - THUDM/GLM-4',
            'models': [],
            'queries': []
        }
        
        # 查询 THUDM/GLM-4 仓库
        try:
            url = "https://api.github.com/repos/THUDM/GLM-4"
            response = requests.get(url, headers=self.headers, timeout=10)
            if response.status_code == 200:
                repo_data = response.json()
                results['queries'].append({
                    'query': 'GitHub Repository: THUDM/GLM-4',
                    'status': 'success',
                    'description': repo_data.get('description', ''),
                    'url': repo_data.get('html_url', ''),
                    'stars': repo_data.get('stargazers_count', 0)
                })
        except Exception as e:
            results['queries'].append({
                'query': 'GitHub Repository: THUDM/GLM-4',
                'status': 'failed',
                'error': str(e)
            })
        
        # 查询模型卡片信息
        try:
            url = "https://huggingface.co/api/models/THUDM/glm-4-9b-chat"
            response = requests.get(url, headers=self.headers, timeout=10)
            if response.status_code == 200:
                model_data = response.json()
                results['models'].append({
                    'name': model_data.get('modelId', ''),
                    'tags': model_data.get('tags', []),
                    'downloads': model_data.get('downloads', 0),
                    'likes': model_data.get('likes', 0),
                    'url': f"https://huggingface.co/{model_data.get('modelId', '')}"
                })
        except Exception as e:
            results['queries'].append({
                'query': 'HuggingFace: GLM-4-9B',
                'status': 'failed',
                'error': str(e)
            })
        
        return results
    
    def search_gpu_benchmarks(self) -> Dict:
        """查询 GPU 性能基准数据"""
        print("🔍 正在查询 GPU 性能基准数据...")
        
        results = {
            'source': 'Various benchmarks',
            'gpus': [],
            'queries': []
        }
        
        # 查询 GPU 性能数据来源
        try:
            # Wikipedia CUDA GPU list
            url = "https://en.wikipedia.org/wiki/CUDA"
            response = requests.get(url, headers=self.headers, timeout=10)
            if response.status_code == 200:
                results['queries'].append({
                    'query': 'Wikipedia CUDA',
                    'status': 'success',
                    'url': url
                })
        except Exception as e:
            results['queries'].append({
                'query': 'Wikipedia CUDA',
                'status': 'failed',
                'error': str(e)
            })
        
        # 添加常见 GPU 规格（基于已知数据）
        common_gpus = [
            {
                'name': 'NVIDIA H100',
                'memory': '80GB HBM3',
                'compute_capability': '9.0',
                'bandwidth': '3.35 TB/s',
                'fp16_performance': '989 TFLOPS',
                'recommended_for': 'All GLM versions'
            },
            {
                'name': 'NVIDIA A100',
                'memory': '40GB/80GB HBM2e',
                'compute_capability': '8.0',
                'bandwidth': '1.55/2.04 TB/s',
                'fp16_performance': '312 TFLOPS',
                'recommended_for': 'GLM-4-9B and below'
            },
            {
                'name': 'NVIDIA A10G',
                'memory': '24GB GDDR6',
                'compute_capability': '8.6',
                'bandwidth': '600 GB/s',
                'fp16_performance': '125 TFLOPS',
                'recommended_for': 'GLM-4-9B (quantized)'
            },
            {
                'name': 'NVIDIA T4',
                'memory': '16GB GDDR6',
                'compute_capability': '7.5',
                'bandwidth': '300 GB/s',
                'fp16_performance': '65 TFLOPS',
                'recommended_for': 'GLM-4-9B (4-bit quantization)'
            },
            {
                'name': 'NVIDIA V100',
                'memory': '16GB/32GB HBM2',
                'compute_capability': '7.0',
                'bandwidth': '900 GB/s',
                'fp16_performance': '125 TFLOPS',
                'recommended_for': 'GLM-4-9B'
            },
            {
                'name': 'NVIDIA RTX 4090',
                'memory': '24GB GDDR6X',
                'compute_capability': '8.9',
                'bandwidth': '1 TB/s',
                'fp16_performance': '83 TFLOPS',
                'recommended_for': 'GLM-4-9B (consumer)'
            },
            {
                'name': 'NVIDIA RTX 3090',
                'memory': '24GB GDDR6X',
                'compute_capability': '8.6',
                'bandwidth': '936 GB/s',
                'fp16_performance': '71 TFLOPS',
                'recommended_for': 'GLM-4-9B (quantized)'
            }
        ]
        
        results['gpus'] = common_gpus
        
        return results
    
    def check_compatibility(self, gpu_name: str, model: str = "glm-4-9b") -> Dict:
        """检查 GPU 与模型的兼容性"""
        gpu_data = self.search_gpu_benchmarks()
        
        gpu_info = None
        for gpu in gpu_data['gpus']:
            if gpu_name.lower() in gpu['name'].lower():
                gpu_info = gpu
                break
        
        if not gpu_info:
            return {
                'status': 'unknown',
                'message': f'未找到 {gpu_name} 的详细数据',
                'recommendation': '请查阅官方文档或手动测试'
            }
        
        # 模型显存需求估算
        model_requirements = {
            'glm-4-9b': {'min_vram': 16, 'recommended_vram': 24},
            'glm-4-9b-chat': {'min_vram': 16, 'recommended_vram': 24},
            'glm-4v-9b': {'min_vram': 20, 'recommended_vram': 32},
            'glm-4-4b': {'min_vram': 8, 'recommended_vram': 12},
        }
        
        req = model_requirements.get(model, {'min_vram': 16, 'recommended_vram': 24})
        
        # 解析 GPU 显存
        gpu_memory_gb = 0
        memory_str = gpu_info['memory']
        if '80GB' in memory_str:
            gpu_memory_gb = 80
        elif '40GB' in memory_str:
            gpu_memory_gb = 40
        elif '32GB' in memory_str:
            gpu_memory_gb = 32
        elif '24GB' in memory_str:
            gpu_memory_gb = 24
        elif '16GB' in memory_str:
            gpu_memory_gb = 16
        elif '12GB' in memory_str:
            gpu_memory_gb = 12
        elif '8GB' in memory_str:
            gpu_memory_gb = 8
        
        # 评估兼容性
        if gpu_memory_gb >= req['recommended_vram']:
            status = 'excellent'
            message = '完全支持，性能良好'
        elif gpu_memory_gb >= req['min_vram']:
            status = 'good'
            message = '可以运行，建议使用 4-bit 或 8-bit 量化'
        else:
            status = 'not_recommended'
            message = '显存不足，建议升级或使用量化版本'
        
        return {
            'gpu': gpu_info,
            'model': model,
            'memory_requirement': req,
            'gpu_memory': gpu_memory_gb,
            'status': status,
            'message': message
        }
    
    def query_all(self, gpu_name: str = None) -> Dict:
        """查询所有相关信息"""
        print("=" * 60)
        print("📊 网络查询工具 - GLM 模型硬件要求查询")
        print("=" * 60)
        print()
        
        results = {
            'timestamp': datetime.now().isoformat(),
            'glm_info': self.search_glm_requirements(),
            'gpu_benchmarks': self.search_gpu_benchmarks()
        }
        
        if gpu_name:
            print(f"\n🔍 检查 GPU 兼容性: {gpu_name}")
            results['compatibility'] = self.check_compatibility(gpu_name)
        
        return results
    
    def print_results(self, results: Dict):
        """格式化打印查询结果"""
        print("\n" + "=" * 60)
        print("📋 查询结果")
        print("=" * 60)
        
        # GLM 模型信息
        print("\n📌 GLM 模型信息:")
        print(f"  来源: {results['glm_info']['source']}")
        for query in results['glm_info']['queries']:
            status_icon = "✅" if query['status'] == 'success' else "❌"
            print(f"  {status_icon} {query['query']}")
            if 'error' in query:
                print(f"      错误: {query['error']}")
        
        if results['glm_info']['models']:
            print("\n  可用模型:")
            for model in results['glm_info']['models']:
                print(f"    • {model['name']}")
                print(f"      下载量: {model['downloads']:,}")
                print(f"      点赞: {model['likes']}")
                print(f"      链接: {model['url']}")
        
        # GPU 性能信息
        print("\n🎮 GPU 性能参考:")
        print(f"  来源: {results['gpu_benchmarks']['source']}")
        for gpu in results['gpu_benchmarks']['gpus']:
            print(f"\n  {gpu['name']}")
            print(f"    显存: {gpu['memory']}")
            print(f"    计算能力: {gpu['compute_capability']}")
            print(f"    带宽: {gpu['bandwidth']}")
            print(f"    FP16 性能: {gpu['fp16_performance']}")
            print(f"    推荐用途: {gpu['recommended_for']}")
        
        # 兼容性检查
        if 'compatibility' in results:
            comp = results['compatibility']
            print("\n" + "=" * 60)
            print(f"🔍 兼容性检查: {comp['gpu']['name']} vs {comp['model']}")
            print("=" * 60)
            
            status_icons = {
                'excellent': '🟢',
                'good': '🟡',
                'not_recommended': '🔴',
                'unknown': '⚪'
            }
            
            print(f"\n  状态: {status_icons.get(comp['status'], '?')} {comp['message']}")
            print(f"\n  GPU 显存: {comp['gpu_memory']} GB")
            print(f"  模型最低要求: {comp['memory_requirement']['min_vram']} GB")
            print(f"  模型推荐显存: {comp['memory_requirement']['recommended_vram']} GB")
            
            if comp['status'] == 'good':
                print("\n  💡 建议:")
                print("    • 使用 4-bit 量化: --load-4bit")
                print("    • 或使用 8-bit 量化: --load-8bit")
                print("    • 减小最大上下文长度")
            elif comp['status'] == 'not_recommended':
                print("\n  ⚠️  替代方案:")
                print("    • 使用 4-bit 量化 + 小上下文")
                print("    • 考虑使用更小的模型（如 GLM-4-4B）")
                print("    • 升级到更大显存的 GPU")
        
        print("\n" + "=" * 60)


def main():
    tool = WebQueryTool()
    
    if len(sys.argv) > 1:
        gpu_name = sys.argv[1]
    else:
        gpu_name = None
    
    results = tool.query_all(gpu_name)
    tool.print_results(results)
    
    # 保存结果到 JSON 文件
    output_file = "/Users/hubertshelley/Documents/silent/tiangong/src-tauri/query_results.json"
    with open(output_file, 'w', encoding='utf-8') as f:
        json.dump(results, f, ensure_ascii=False, indent=2)
    
    print(f"\n💾 查询结果已保存到: {output_file}")


if __name__ == '__main__':
    main()

import { describe, expect, it } from 'vitest';
import type { ToolInvocation } from '@tiangong/plugin-sdk';

import {
  USER_TIMEOUT_MS,
  parseInvocation,
  payloadResult,
} from './interaction';

function invocation(argumentsValue: Record<string, unknown>): ToolInvocation {
  return {
    invocation_id: 'invocation-1',
    session_id: 'session-1',
    tool_call_id: 'tool-call-1',
    name: 'request_user',
    arguments: argumentsValue,
    created_at: '2026-08-18T20:00:00.000',
  };
}

describe('interaction-handler 业务边界', () => {
  it('由插件按调用创建时间独立计算 15 秒截止时间', () => {
    const request = parseInvocation(invocation({
      kind: 'approval',
      title: '是否继续',
    }));

    expect(request.deadlineMs - request.createdAtMs).toBe(USER_TIMEOUT_MS);
    expect(USER_TIMEOUT_MS).toBe(15_000);
  });

  it('审批不需要宿主挑战并以普通工具结果返回用户意见', () => {
    const request = parseInvocation(invocation({
      kind: 'approval',
      title: '是否继续',
    }));
    const result = payloadResult(
      request.invocationId,
      request.kind,
      'answered',
      { result: { decision: 'approve_once' } },
      true,
    );

    expect(JSON.parse(result.summary)).toEqual({
      status: 'answered',
      kind: 'approval',
      request_id: 'invocation-1',
      result: { decision: 'approve_once' },
    });
    expect(result).toEqual({
      ok: true,
      summary: result.summary,
      stdout: '',
      stderr: '',
      exit_code: 0,
    });
  });

  it('选择和表单参数规则完全由插件校验', () => {
    expect(() => parseInvocation(invocation({
      kind: 'choice',
      title: '请选择',
    }))).toThrow('options 必须是非空字符串数组');

    expect(() => parseInvocation(invocation({
      kind: 'form',
      title: '请填写',
      fields: [
        { key: 'name', label: '名称', type: 'string' },
        { key: 'name', label: '重复名称', type: 'string' },
      ],
    }))).toThrow('fields 包含重复 key：name');
  });
});

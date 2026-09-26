export const rotationOptions = [
  {
    label: '智能调度（推荐）',
    value: 'smart',
    description: '综合负载、剩余额度、健康和延迟评分，支持自定义偏好与权重回切',
  },

  {
    label: '额度重置优先',
    value: 'quota_reset_priority',
    description: '优先选择额度窗口更快重置的账号，适合在重置前消耗剩余额度',
  },
  {
    label: '轮询调度',
    value: 'round_robin',
    description: '在可用候选账号间按顺序轮转，分配结果最可预测',
  },
  {
    label: '粘性调度',
    value: 'sticky',
    description: '优先复用最近使用的账号，直到不可用后再切换',
  },
] as const

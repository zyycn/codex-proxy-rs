export const rotationOptions = [
  {
    label: '智能调度（推荐）',
    value: 'smart',
    description: '按负载、窗口用量、请求数和健康反馈评分，优先选择更空闲的账号',
  },
  {
    label: '权重优先',
    value: 'weight_priority',
    description: '优先使用可用的高权重账号，满载时向低权重分流，恢复可用后优先切回',
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

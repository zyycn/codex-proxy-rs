<script setup lang="ts">
import { BaseCard, BaseSwitch } from '@codex-proxy/ui'

defineProps<{ disabled: boolean }>()
const stripWorkspaces = defineModel<boolean>('stripWorkspaces', { required: true })
</script>

<template>
  <BaseCard
    title="Codex 轮次元信息"
    description="删除发往上游的 turn 元数据中的 workspaces 字段，避免本地工作区路径与仓库信息随请求外发"
  >
    <template #body>
      <div>
        <BaseSwitch
          v-model="stripWorkspaces"
          label="删除 workspaces 字段"
          show-label
          :disabled="disabled"
        />
      </div>
    </template>
  </BaseCard>
</template>

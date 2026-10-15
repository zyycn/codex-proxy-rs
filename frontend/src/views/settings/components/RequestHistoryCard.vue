<script setup lang="ts">
import { ZCard, ZForm, ZFormItem, ZInput } from '@codex-proxy/ui'

const model = defineModel<string>({ required: true })
</script>

<template>
  <ZCard title="历史保留" description="自动清理过期请求及对应统计">
    <ZForm class="max-w-6xl sm:grid-cols-2 sm:items-end">
      <ZFormItem label="保留天数" description="至少 1 天，过期数据自动分批清理，清理后无法恢复">
        <ZInput v-model="model" aria-label="历史保留天数" type="number" min="1" max="31" step="1" placeholder="请输入 1–31 天">
          <template #suffix>
            天
          </template>
        </ZInput>
      </ZFormItem>
    </ZForm>
  </ZCard>
</template>

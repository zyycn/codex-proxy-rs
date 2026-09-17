<script setup lang="ts">
import { Timer, Zap } from '@lucide/vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'

const refreshMarginSeconds = defineModel<string>('refreshMarginSeconds', { required: true })
const refreshConcurrency = defineModel<string>('refreshConcurrency', { required: true })
</script>

<template>
  <BaseCard title="凭据自动刷新">
    <BaseForm class="max-w-6xl sm:grid-cols-2">
      <BaseFormItem label="提前刷新秒数" description="Token 过期前多少秒触发刷新">
        <BaseInput v-model="refreshMarginSeconds" aria-label="提前刷新秒数" type="number">
          <template #prefix>
            <Timer class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>
      <BaseFormItem label="刷新并发数" description="同时刷新 Token 的最大请求数，减小可避免限流">
        <BaseInput v-model="refreshConcurrency" aria-label="刷新并发数" type="number">
          <template #prefix>
            <Zap class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>

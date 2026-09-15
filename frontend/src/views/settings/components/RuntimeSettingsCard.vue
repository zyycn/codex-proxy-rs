<script setup lang="ts">
import { Gauge, Timer, Zap } from '@lucide/vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'

const maxConcurrentPerAccount = defineModel<string>('maxConcurrentPerAccount', { required: true })
const refreshMarginSeconds = defineModel<string>('refreshMarginSeconds', { required: true })
const refreshConcurrency = defineModel<string>('refreshConcurrency', { required: true })
const requestIntervalMs = defineModel<string>('requestIntervalMs', { required: true })
const maxWaitingPerKey = defineModel<string>('maxWaitingPerKey', { required: true })
const maxWaitingPerAccount = defineModel<string>('maxWaitingPerAccount', { required: true })
const concurrencyWaitTimeoutSeconds = defineModel<string>('concurrencyWaitTimeoutSeconds', { required: true })
</script>

<template>
  <BaseCard
    title="运行参数"
    description="请求节奏、账号并发和 Token 刷新"
  >
    <BaseForm class="max-w-6xl sm:grid-cols-2">
      <BaseFormItem
        label="单账号默认最大并发"
        description="账号未单独设置时使用的并发上限"
      >
        <BaseInput
          v-model="maxConcurrentPerAccount"
          aria-label="单账号默认最大并发"
          type="number"
        >
          <template #prefix>
            <Gauge class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="提前刷新秒数"
        description="Token 过期前多少秒触发刷新"
      >
        <BaseInput
          v-model="refreshMarginSeconds"
          aria-label="提前刷新秒数"
          type="number"
        >
          <template #prefix>
            <Timer class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="刷新并发数"
        description="同时刷新 Token 的最大请求数，减小可避免限流"
      >
        <BaseInput
          v-model="refreshConcurrency"
          aria-label="刷新并发数"
          type="number"
        >
          <template #prefix>
            <Zap class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="请求间隔 ms"
        description="控制同一账号两次调度之间的最小等待时间"
      >
        <BaseInput
          v-model="requestIntervalMs"
          aria-label="请求间隔 ms"
          type="number"
        >
          <template #prefix>
            <Timer class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>
      <div class="col-span-full grid gap-4 pt-4 sm:grid-cols-2">
        <BaseFormItem label="每个密钥最大排队数" description="所有密钥使用此上限，各自独立计数；0 表示不排队">
          <BaseInput v-model="maxWaitingPerKey" aria-label="每个密钥最大排队数" type="number" min="0" max="1000" step="1" />
        </BaseFormItem>

        <BaseFormItem label="单账号最大排队数" description="所有账号使用此上限，各自独立计数；0 表示不排队">
          <BaseInput v-model="maxWaitingPerAccount" aria-label="单账号最大排队数" type="number" min="0" max="1000" step="1" />
        </BaseFormItem>

        <BaseFormItem label="最长排队秒数" description="密钥和账号共用此时限，从首次入队开始计时，范围 1～120 秒">
          <BaseInput v-model="concurrencyWaitTimeoutSeconds" aria-label="最长排队秒数" type="number" min="1" max="120" step="1" />
        </BaseFormItem>
      </div>
    </BaseForm>
  </BaseCard>
</template>

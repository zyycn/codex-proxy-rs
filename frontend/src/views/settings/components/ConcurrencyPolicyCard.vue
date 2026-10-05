<script setup lang="ts">
import { BaseCard, BaseForm, BaseFormItem, BaseInput, BasePopover } from '@codex-proxy/ui'

import { CircleAlert, Gauge, Timer } from '@lucide/vue'

const maxConcurrentPerAccount = defineModel<string>('maxConcurrentPerAccount', { required: true })
const requestIntervalMs = defineModel<string>('requestIntervalMs', { required: true })
const maxWaitingPerKey = defineModel<string>('maxWaitingPerKey', { required: true })
const maxWaitingPerAccount = defineModel<string>('maxWaitingPerAccount', { required: true })
const concurrencyWaitTimeoutSeconds = defineModel<string>('concurrencyWaitTimeoutSeconds', { required: true })
const openaiGuardianReservedConcurrency = defineModel<string>('openaiGuardianReservedConcurrency', { required: true })
</script>

<template>
  <BaseCard title="并发策略">
    <BaseForm class="max-w-6xl sm:grid-cols-2">
      <BaseFormItem
        label="默认账号并发上限"
        description="账号未单独设置时使用的并发上限，0 表示不限制"
      >
        <BaseInput
          v-model="maxConcurrentPerAccount"
          aria-label="默认账号并发上限"
          type="number"
          min="0"
          max="4294967295"
          step="1"
        >
          <template #prefix>
            <Gauge class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="最小请求间隔"
        description="控制同一账号两次调度之间的最小等待时间"
      >
        <BaseInput
          v-model="requestIntervalMs"
          aria-label="最小请求间隔（毫秒）"
          type="number"
        >
          <template #prefix>
            <Timer class="size-4" />
          </template>
          <template #suffix>
            毫秒
          </template>
        </BaseInput>
      </BaseFormItem>
      <BaseFormItem label="密钥队列容量" description="每个客户端密钥允许等待的最大请求数，0 表示不排队">
        <BaseInput v-model="maxWaitingPerKey" aria-label="密钥队列容量" type="number" min="0" max="1000" step="1" />
      </BaseFormItem>
      <BaseFormItem label="账号队列容量" description="每个上游账号允许等待的最大请求数，0 表示不排队">
        <BaseInput v-model="maxWaitingPerAccount" aria-label="账号队列容量" type="number" min="0" max="1000" step="1" />
      </BaseFormItem>
      <BaseFormItem label="排队超时（秒）" description="密钥队列与账号队列共用的等待时限，从首次入队起计时，1～120 秒">
        <BaseInput v-model="concurrencyWaitTimeoutSeconds" aria-label="排队超时（秒）" type="number" min="1" max="120" step="1" />
      </BaseFormItem>
      <BaseFormItem label="自动审批预留并发" description="为 Codex 自动审批预留每账号名额，0 表示关闭">
        <template #label-extra>
          <BasePopover class="-my-1" trigger="hover-click" placement="top-start">
            <template #trigger="{ open }">
              <button type="button" aria-label="自动审批预留并发说明" :aria-expanded="open" class="inline-flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-cp-sm border-0 bg-transparent p-0 text-cp-text-tertiary outline-none transition-colors hover:text-cp-text focus-visible:ring-2 focus-visible:ring-cp-control-outline motion-reduce:transition-none">
                <CircleAlert class="size-3.5" aria-hidden="true" />
              </button>
            </template>
            <div class="max-w-72 space-y-2 px-3 py-2 text-cp-sm leading-relaxed text-cp-text-secondary">
              <p>仅对 OpenAI 账号生效，普通请求可用并发会相应减少，至少保留 1 个名额，不限并发的账号不扣减</p>
              <p>大于 0 且开启账号排队时，审批请求优先等待，可超过账号队列容量，仍受总等待容量、排队超时与密钥限额约束</p>
            </div>
          </BasePopover>
        </template>
        <BaseInput v-model="openaiGuardianReservedConcurrency" aria-label="自动审批预留并发" type="number" min="0" max="4294967295" step="1" />
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>

<script setup lang="ts">
import { BaseCard, BaseForm, BaseFormItem, BaseInput, BaseSwitch } from '@codex-proxy/ui'
import { Clock, Sparkles } from '@lucide/vue'

const enabled = defineModel<boolean>('enabled', { required: true })
const scheduleTime = defineModel<string>('scheduleTime', { required: true })
const model = defineModel<string>('model', { required: true })
</script>

<template>
  <BaseCard
    title="账号预激活"
    description="在指定时间向空闲的 Codex OAuth 账号发送一次请求，尝试启动额度窗口"
  >
    <BaseForm class="max-w-6xl sm:grid-cols-2">
      <BaseSwitch
        v-model="enabled"
        class="col-span-full justify-self-start"
        label="启用每日预激活"
        show-label
      />

      <BaseFormItem
        label="执行时间"
        description="HH:MM；多个时间用英文逗号分隔"
      >
        <BaseInput
          v-model="scheduleTime"
          :disabled="!enabled"
          aria-label="每日激活时间"
          placeholder="08:00"
        >
          <template #prefix>
            <Clock class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="预激活模型"
        description="启用时必填，请填写账号可用的模型"
        :required="enabled"
      >
        <BaseInput
          v-model="model"
          :disabled="!enabled"
          aria-label="预激活模型"
          placeholder="请输入模型名称"
        >
          <template #prefix>
            <Sparkles class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>

<script setup lang="ts">
import { CircleHelp, MonitorUp, TerminalSquare } from '@lucide/vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'

defineOptions({ name: 'ClientVersionPolicyCard' })

withDefaults(
  defineProps<{
    loading?: boolean
    desktopError?: string
    cliError?: string
  }>(),
  {
    loading: false,
    desktopError: '',
    cliError: '',
  },
)

const emit = defineEmits<{
  help: []
}>()

const minCodexDesktopVersion = defineModel<string>('minCodexDesktopVersion', { required: true })
const minCodexCliVersion = defineModel<string>('minCodexCliVersion', { required: true })
</script>

<template>
  <BaseCard>
    <template #title>
      <span class="inline-flex items-center gap-1.5">
        <span>客户端版本限制</span>
        <button
          type="button"
          class="inline-flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-cp-sm border-0 bg-transparent p-0 text-cp-text-tertiary outline-none transition-colors hover:text-cp-text focus-visible:ring-2 focus-visible:ring-cp-control-outline motion-reduce:transition-none"
          aria-label="查看安装与升级说明"
          @click="emit('help')"
        >
          <CircleHelp class="size-3.5" aria-hidden="true" />
        </button>
      </span>
    </template>

    <BaseForm class="max-w-6xl sm:grid-cols-2">
      <BaseFormItem
        label="Codex Desktop 最低版本"
        description="只检查桌面端应用版本，例如 26.825.51511"
        :error="desktopError"
      >
        <BaseInput
          v-model="minCodexDesktopVersion"
          aria-label="Codex Desktop 最低版本"
          autocomplete="off"
          spellcheck="false"
          placeholder="不限制"
          :disabled="loading"
        >
          <template #prefix>
            <MonitorUp class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>

      <BaseFormItem
        label="Codex CLI 最低版本"
        description="只检查独立终端版本，例如 0.152.0"
        :error="cliError"
      >
        <BaseInput
          v-model="minCodexCliVersion"
          aria-label="Codex CLI 最低版本"
          autocomplete="off"
          spellcheck="false"
          placeholder="不限制"
          :disabled="loading"
        >
          <template #prefix>
            <TerminalSquare class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>

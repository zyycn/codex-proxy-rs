<script setup lang="ts">
import { Apple, Copy, Monitor } from '@lucide/vue'
import { computed, shallowRef } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'

const props = defineProps<{
  apiKey: {
    name?: string
    key?: string
    providerKind?: string
  } | null
  apiBaseUrl: string
}>()

const emit = defineEmits<{
  copy: [text: string]
}>()

const open = defineModel<boolean>({ default: false })

const activePlatform = shallowRef('unix')

const platformOptions = [
  { label: 'macOS / Linux', value: 'unix', icon: Apple },
  { label: 'Windows', value: 'windows', icon: Monitor },
]

const keyValue = computed(() => props.apiKey?.key ?? '')
const configPath = computed(() =>
  activePlatform.value === 'windows'
    ? '%userprofile%\\.codex\\config.toml'
    : '~/.codex/config.toml',
)
const authPath = computed(() =>
  activePlatform.value === 'windows' ? '%userprofile%\\.codex\\auth.json' : '~/.codex/auth.json',
)
const codexAuthJson = computed(() => JSON.stringify({ OPENAI_API_KEY: keyValue.value }, null, 2))
const defaultModel = computed(() =>
  props.apiKey?.providerKind?.trim().toLowerCase() === 'xai' ? 'grok-4.5' : 'gpt-5.5',
)

const codexConfigToml = computed(
  () => `model_provider = "OpenAI"
model = "${defaultModel.value}"
review_model = "${defaultModel.value}"
model_reasoning_effort = "xhigh"
disable_response_storage = true
network_access = "enabled"
windows_wsl_setup_acknowledged = true

[model_providers.OpenAI]
name = "OpenAI"
base_url = "${props.apiBaseUrl}"
wire_api = "responses"
requires_openai_auth = true

[features]
goals = true`,
)

const visibleFiles = computed(() => [
  { path: configPath.value, content: codexConfigToml.value },
  { path: authPath.value, content: codexAuthJson.value },
])
</script>

<template>
  <BaseModal
    v-model="open"
    title="使用密钥"
    description="将以下配置文件添加到 Codex CLI 配置目录中"
    width="760px"
  >
    <div class="flex flex-col gap-5">
      <div class="flex flex-wrap items-center gap-3">
        <BaseSegmented v-model="activePlatform" :options="platformOptions" />
      </div>

      <div class="flex flex-col gap-3">
        <section
          v-for="file in visibleFiles"
          :key="file.path"
          class="overflow-hidden rounded-(--cp-card-radius) bg-(--cp-bg-subtle) shadow-(--cp-shadow-control)"
        >
          <div class="flex items-center justify-between gap-3 px-4 py-2.5">
            <span
              class="min-w-0 truncate font-mono text-[12px] font-[650] text-(--cp-text-secondary)"
            >
              {{ file.path }}
            </span>
            <BaseButton
              icon-only
              variant="default"
              size="sm"
              label="复制"
              @click="emit('copy', file.content)"
            >
              <Copy class="size-3.5" />
            </BaseButton>
          </div>
          <BaseScrollbar
            max-height="360px"
            view-class="mx-3 mb-3 rounded-(--cp-input-radius-base) bg-(--cp-bg-surface) px-3.5 py-3 shadow-(--cp-shadow-input)"
          >
            <pre
              class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[12px] leading-[1.65] font-[650] text-(--cp-text-primary)"
            >{{ file.content }}</pre>
          </BaseScrollbar>
        </section>
      </div>
    </div>

    <template #footer>
      <BaseButton variant="primary" @click="open = false">
        关闭
      </BaseButton>
    </template>
  </BaseModal>
</template>

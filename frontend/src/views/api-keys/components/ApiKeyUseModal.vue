<script setup lang="ts">
import { Copy } from '@lucide/vue'
import { computed, shallowRef } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'

const props = defineProps<{
  apiKey: {
    name?: string
    key?: string
  } | null
  serviceRootUrl: string
  apiBaseUrl: string
}>()

const open = defineModel<boolean>({ default: false })

const emit = defineEmits<{
  copy: [text: string]
}>()

const activeTab = shallowRef('codex')

const tabOptions = [
  { label: 'Codex', value: 'codex' },
  { label: '环境变量', value: 'env' },
  { label: 'curl', value: 'curl' },
]

const keyValue = computed(() => props.apiKey?.key ?? '')
const displayName = computed(() => props.apiKey?.name || 'API Key')
const codexAuthJson = computed(() => JSON.stringify({ OPENAI_API_KEY: keyValue.value }, null, 2))

const codexConfigToml = computed(
  () => `model_provider = "codex-proxy-rs"
model = "gpt-5.5"
review_model = "gpt-5.5"
model_reasoning_effort = "xhigh"
disable_response_storage = true

[model_providers.codex-proxy-rs]
name = "codex-proxy-rs"
base_url = "${props.apiBaseUrl}"
wire_api = "responses"
requires_openai_auth = true`,
)

const envConfig = computed(
  () => `export OPENAI_API_KEY="${keyValue.value}"
export OPENAI_BASE_URL="${props.apiBaseUrl}"`,
)

const curlExample = computed(
  () => `curl "${props.apiBaseUrl}/responses" \\
  -H "Authorization: Bearer ${keyValue.value}" \\
  -H "Content-Type: application/json" \\
  -d '{
    "model": "gpt-5.5",
    "input": "ping",
    "stream": false
  }'`,
)

const visibleFiles = computed(() => {
  if (activeTab.value === 'env') {
    return [{ path: 'Shell', content: envConfig.value }]
  }

  if (activeTab.value === 'curl') {
    return [{ path: '/v1/responses', content: curlExample.value }]
  }

  return [
    { path: '~/.codex/config.toml', content: codexConfigToml.value },
    { path: '~/.codex/auth.json', content: codexAuthJson.value },
  ]
})
</script>

<template>
  <BaseModal v-model="open" title="使用密钥" :description="displayName" width="760px">
    <div class="flex flex-col gap-5">
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <section class="rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3.5">
          <p class="m-0 text-[11px] leading-none font-bold text-(--cp-text-muted)">服务根地址</p>
          <div class="mt-2 flex min-w-0 items-center gap-2">
            <code
              class="min-w-0 flex-1 truncate font-mono text-[12px] font-[650] text-(--cp-text-primary)"
              :title="serviceRootUrl"
            >
              {{ serviceRootUrl }}
            </code>
            <BaseButton
              icon-only
              variant="ghost"
              size="sm"
              label="复制服务根地址"
              @click="emit('copy', serviceRootUrl)"
            >
              <Copy class="size-3.5" />
            </BaseButton>
          </div>
        </section>

        <section class="rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3.5">
          <p class="m-0 text-[11px] leading-none font-bold text-(--cp-text-muted)">
            OpenAI Base URL
          </p>
          <div class="mt-2 flex min-w-0 items-center gap-2">
            <code
              class="min-w-0 flex-1 truncate font-mono text-[12px] font-[650] text-(--cp-text-primary)"
              :title="apiBaseUrl"
            >
              {{ apiBaseUrl }}
            </code>
            <BaseButton
              icon-only
              variant="ghost"
              size="sm"
              label="复制 Base URL"
              @click="emit('copy', apiBaseUrl)"
            >
              <Copy class="size-3.5" />
            </BaseButton>
          </div>
        </section>
      </div>

      <section class="rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3.5">
        <p class="m-0 text-[11px] leading-none font-bold text-(--cp-text-muted)">Bearer Key</p>
        <div class="mt-2 flex min-w-0 items-center gap-2">
          <code
            class="min-w-0 flex-1 truncate font-mono text-[12px] font-[650] text-(--cp-text-primary)"
            :title="keyValue"
          >
            {{ keyValue }}
          </code>
          <BaseButton
            icon-only
            variant="ghost"
            size="sm"
            label="复制 Bearer Key"
            @click="emit('copy', keyValue)"
          >
            <Copy class="size-3.5" />
          </BaseButton>
        </div>
      </section>

      <div class="flex flex-wrap items-center gap-3">
        <BaseSegmented v-model="activeTab" :options="tabOptions" />
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
            max-height="180px"
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
      <BaseButton variant="primary" @click="open = false">关闭</BaseButton>
    </template>
  </BaseModal>
</template>

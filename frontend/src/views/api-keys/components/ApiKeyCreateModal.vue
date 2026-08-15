<script setup lang="ts">
import type { ApiKeyFormValue } from '../composables/useApiKeyMutations'
import type { AccountGroup } from '@/api'
import { Copy, Upload } from '@lucide/vue'
import { computed } from 'vue'

import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'

const props = defineProps<{
  groups: AccountGroup[]
  groupLoading: boolean
  editing: boolean
  createdKey: string
  saving: boolean
}>()
const emit = defineEmits<{
  save: []
  copy: [text: string]
  importCcs: []
}>()
const open = defineModel<boolean>({ default: false })
const createdOpen = defineModel<boolean>('createdOpen', { default: false })
const form = defineModel<ApiKeyFormValue>('form', { required: true })
const title = computed(() => props.editing ? '编辑 API Key' : '创建 API Key')
</script>

<template>
  <BaseModal
    v-model="open"
    :title="title"
    description="设置调用方可用的账号分组、并发和每分钟请求上限"
    tone="info"
    size="lg"
    :dismissible="!saving"
  >
    <BaseForm class="grid gap-5">
      <BaseFormItem label="名称" required>
        <BaseInput
          v-model="form.name"
          aria-label="名称"
          placeholder="例如：生产环境"
          :disabled="saving"
        />
      </BaseFormItem>

      <BaseFormItem label="标签（可选）">
        <BaseInput
          v-model="form.label"
          aria-label="标签（可选）"
          placeholder="例如：后端服务"
          :disabled="saving"
        />
      </BaseFormItem>

      <BaseFormItem label="分组">
        <AccountGroupCheckboxGrid
          v-model="form.groupIds"
          :groups="groups"
          :loading="groupLoading"
          :disabled="saving"
        />
      </BaseFormItem>

      <div class="grid gap-4 sm:grid-cols-2">
        <BaseFormItem label="最大并发" description="0 表示不限制">
          <BaseInput
            v-model="form.maxConcurrency"
            type="number"
            aria-label="最大并发"
            :disabled="saving"
          />
        </BaseFormItem>
        <BaseFormItem label="每分钟请求数" description="0 表示不限制">
          <BaseInput
            v-model="form.requestsPerMinute"
            type="number"
            aria-label="每分钟请求数"
            :disabled="saving"
          />
        </BaseFormItem>
      </div>
    </BaseForm>

    <template #footer>
      <BaseButton variant="ghost" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton
        variant="primary"
        :loading="saving"
        :disabled="!form.name.trim()"
        @click="emit('save')"
      >
        {{ editing ? '保存更改' : '创建' }}
      </BaseButton>
    </template>
  </BaseModal>

  <BaseModal
    v-model="createdOpen"
    title="API Key 已创建"
    description="复制密钥，或直接导入 CCSwitch"
    tone="success"
    size="md"
  >
    <div class="flex flex-col gap-4">
      <div class="rounded-cp-control border border-cp-warning-border bg-cp-warning-bg px-4 py-3">
        <p class="m-0 text-[13px] font-semibold text-cp-warning-text">
          该密钥具有网关访问权限，请仅发送给可信调用方
        </p>
      </div>
      <div>
        <p class="mb-2 text-[13px] font-medium text-cp-secondary">
          API Key
        </p>
        <div class="flex items-center gap-2">
          <code class="flex-1 rounded-cp-control bg-cp-subtle px-3 py-2.5 font-mono text-[13px] break-all text-cp-primary">
            {{ createdKey }}
          </code>
          <BaseIconButton size="md" label="复制" @click="emit('copy', createdKey)">
            <Copy class="size-4" />
          </BaseIconButton>
        </div>
      </div>
    </div>

    <template #footer>
      <BaseButton variant="secondary" @click="emit('copy', createdKey)">
        <template #icon>
          <Copy class="size-4" />
        </template>
        复制密钥
      </BaseButton>
      <BaseButton variant="secondary" @click="emit('importCcs')">
        <template #icon>
          <Upload class="size-4" />
        </template>
        导入 CCSwitch
      </BaseButton>
      <BaseButton variant="primary" @click="createdOpen = false">
        我已保存
      </BaseButton>
    </template>
  </BaseModal>
</template>

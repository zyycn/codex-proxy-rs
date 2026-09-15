<script setup lang="ts">
import type { ApiKeyFormValue } from '../composables/useApiKeyMutations'
import type { AccountGroup } from '@/api'
import { Copy, DollarSign, KeyRound, Upload } from '@lucide/vue'
import { computed } from 'vue'

import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'

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
    tone="info"
    size="md"
    :dismissible="!saving"
  >
    <template #icon>
      <KeyRound class="text-cp-text" :size="20" aria-hidden="true" />
    </template>

    <BaseForm class="grid gap-6">
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

      <BaseFormItem
        v-if="!editing"
        label="自定义 Key（可选）"
      >
        <BaseInput
          v-model="form.customKey"
          type="password"
          autocomplete="new-password"
          :spellcheck="false"
          aria-label="自定义 Key（可选）"
          placeholder="留空自动生成"
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

      <div class="grid gap-6 sm:grid-cols-2">
        <BaseFormItem label="日限额">
          <BaseInput
            v-model="form.dailyLimitUsd"
            type="number"
            min="0"
            step="any"
            aria-label="日限额（美元）"
            placeholder="不限制"
            :disabled="saving"
          >
            <template #prefix>
              <DollarSign class="size-4" aria-hidden="true" />
            </template>
          </BaseInput>
        </BaseFormItem>
        <BaseFormItem label="周限额">
          <BaseInput
            v-model="form.weeklyLimitUsd"
            type="number"
            min="0"
            step="any"
            aria-label="周限额（美元）"
            placeholder="不限制"
            :disabled="saving"
          >
            <template #prefix>
              <DollarSign class="size-4" aria-hidden="true" />
            </template>
          </BaseInput>
        </BaseFormItem>
      </div>

      <div class="grid gap-6 sm:grid-cols-2">
        <BaseFormItem label="最大并发">
          <BaseInput
            v-model="form.maxConcurrency"
            type="number"
            aria-label="最大并发"
            min="0"
            step="1"
            placeholder="不限制"
            :disabled="saving"
          />
        </BaseFormItem>
        <BaseFormItem label="每分钟请求数（RPM）">
          <BaseInput
            v-model="form.requestsPerMinute"
            type="number"
            aria-label="每分钟请求数（RPM）"
            min="0"
            step="1"
            placeholder="不限制"
            :disabled="saving"
          />
        </BaseFormItem>
      </div>
    </BaseForm>

    <template #footer>
      <BaseButton variant="secondary" :disabled="saving" @click="open = false">
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
      <div class="rounded-cp border border-cp-warning-border bg-cp-warning-container px-4 py-3">
        <p class="m-0 text-cp font-semibold text-cp-warning-on-container">
          该密钥具有网关访问权限，请仅发送给可信调用方
        </p>
      </div>
      <div>
        <p class="mb-2 text-cp font-medium text-cp-text-secondary">
          API Key
        </p>
        <div class="flex items-center gap-2">
          <code class="flex-1 rounded-cp bg-cp-fill-quaternary px-3 py-2.5 font-mono text-cp break-all text-cp-text">
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

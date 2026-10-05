<script setup lang="ts">
import type { ApiKeyFormValue } from '../composables/useApiKeyEditor'
import type { AccountGroup } from '@/api'
import { BaseButton, BaseForm, BaseFormItem, BaseInput, BaseModal } from '@codex-proxy/ui'

import { DollarSign, KeyRound } from '@lucide/vue'
import { computed } from 'vue'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import ProviderRequestProfilesEditor from '@/components/client-profile/ProviderRequestProfilesEditor.vue'

const props = defineProps<{
  groups: AccountGroup[]
  groupLoading: boolean
  editing: boolean
  saving: boolean
}>()
const emit = defineEmits<{
  save: []
  afterLeave: []
}>()
const open = defineModel<boolean>({ default: false })
const form = defineModel<ApiKeyFormValue>('form', { required: true })
const title = computed(() => props.editing ? '编辑密钥' : '创建 API Key')

function updateForm<Key extends keyof ApiKeyFormValue>(key: Key, value: ApiKeyFormValue[Key]) {
  form.value = { ...form.value, [key]: value }
}
</script>

<template>
  <BaseModal
    v-model="open"
    :title="title"
    description="配置密钥信息、分组与使用限制"
    tone="info"
    size="lg"
    :dismissible="!saving"
    @after-leave="emit('afterLeave')"
  >
    <template #icon>
      <KeyRound class="text-cp-text" :size="20" aria-hidden="true" />
    </template>

    <BaseForm class="grid gap-6">
      <BaseFormItem label="名称" required>
        <BaseInput
          :model-value="form.name" aria-label="名称"
          placeholder="例如：生产环境"
          :disabled="saving"
          @update:model-value="updateForm('name', $event)"
        />
      </BaseFormItem>

      <div class="grid gap-6" :class="{ 'sm:grid-cols-2': !editing }">
        <BaseFormItem label="标签（可选）">
          <BaseInput
            :model-value="form.label" aria-label="标签（可选）"
            placeholder="例如：后端服务"
            :disabled="saving"
            @update:model-value="updateForm('label', $event)"
          />
        </BaseFormItem>

        <BaseFormItem
          v-if="!editing"
          label="自定义 Key（可选）"
        >
          <BaseInput
            :model-value="form.customKey" type="password"
            autocomplete="new-password"
            :spellcheck="false"
            aria-label="自定义 Key（可选）"
            placeholder="留空自动生成"
            :disabled="saving"
            @update:model-value="updateForm('customKey', $event)"
          />
        </BaseFormItem>
      </div>

      <BaseFormItem label="分组">
        <AccountGroupCheckboxGrid
          :model-value="form.groupIds" :groups="groups"
          :loading="groupLoading"
          :disabled="saving"
          @update:model-value="updateForm('groupIds', $event)"
        />
      </BaseFormItem>

      <BaseFormItem label="上游身份">
        <ProviderRequestProfilesEditor
          :model-value="form.providerRequestProfileOverrides" allow-inherit
          :active="open"
          :disabled="saving"
          @update:model-value="updateForm('providerRequestProfileOverrides', $event)"
        />
      </BaseFormItem>

      <div class="grid gap-6 sm:grid-cols-2">
        <BaseFormItem label="日限额">
          <BaseInput
            :model-value="form.dailyLimitUsd" type="number"
            min="0"
            step="any"
            aria-label="日限额（美元）"
            placeholder="不限制"
            :disabled="saving"
            @update:model-value="updateForm('dailyLimitUsd', $event)"
          >
            <template #prefix>
              <DollarSign class="size-4" aria-hidden="true" />
            </template>
          </BaseInput>
        </BaseFormItem>
        <BaseFormItem label="周限额">
          <BaseInput
            :model-value="form.weeklyLimitUsd" type="number"
            min="0"
            step="any"
            aria-label="周限额（美元）"
            placeholder="不限制"
            :disabled="saving"
            @update:model-value="updateForm('weeklyLimitUsd', $event)"
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
            :model-value="form.maxConcurrency" type="number"
            aria-label="最大并发"
            min="0"
            step="1"
            placeholder="不限制"
            :disabled="saving"
            @update:model-value="updateForm('maxConcurrency', $event)"
          />
        </BaseFormItem>
        <BaseFormItem label="每分钟请求数（RPM）">
          <BaseInput
            :model-value="form.requestsPerMinute" type="number"
            aria-label="每分钟请求数（RPM）"
            min="0"
            step="1"
            placeholder="不限制"
            :disabled="saving"
            @update:model-value="updateForm('requestsPerMinute', $event)"
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
</template>

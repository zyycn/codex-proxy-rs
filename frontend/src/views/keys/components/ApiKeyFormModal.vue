<script setup lang="ts">
import type { ApiKeyFormValue } from '../composables/useApiKeyEditor'
import type { AccountGroupRef } from '@/api'
import { ZButton, ZDialog, ZForm, ZFormItem, ZInput, ZSwitch } from '@codex-proxy/ui'

import { DollarSign, KeyRound } from '@lucide/vue'
import { computed } from 'vue'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import ProviderRequestProfilesEditor from '@/components/client-profile/ProviderRequestProfilesEditor.vue'

const props = defineProps<{
  groups: AccountGroupRef[]
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
  <ZDialog
    v-model="open"
    :title="title"
    description="配置密钥信息、分组与使用限制"
    type="info"
    width="48rem"
    :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving"
    @closed="emit('afterLeave')"
  >
    <template #icon>
      <KeyRound class="text-cp-text" :size="20" aria-hidden="true" />
    </template>

    <ZForm class="grid gap-6">
      <ZFormItem label="名称" required>
        <ZInput
          :model-value="form.name" aria-label="名称"
          placeholder="例如：生产环境"
          :disabled="saving"
          @update:model-value="updateForm('name', $event)"
        />
      </ZFormItem>

      <div class="grid gap-6" :class="{ 'sm:grid-cols-2': !editing }">
        <ZFormItem label="标签（可选）">
          <ZInput
            :model-value="form.label" aria-label="标签（可选）"
            placeholder="例如：后端服务"
            :disabled="saving"
            @update:model-value="updateForm('label', $event)"
          />
        </ZFormItem>

        <ZFormItem
          v-if="!editing"
          label="自定义 Key（可选）"
        >
          <ZInput
            :model-value="form.customKey" type="password"
            autocomplete="new-password"
            :spellcheck="false"
            aria-label="自定义 Key（可选）"
            placeholder="留空自动生成"
            :disabled="saving"
            @update:model-value="updateForm('customKey', $event)"
          />
        </ZFormItem>
      </div>

      <ZFormItem label="分组">
        <AccountGroupCheckboxGrid
          :model-value="form.groupIds" :groups="groups"
          :loading="groupLoading"
          :disabled="saving"
          @update:model-value="updateForm('groupIds', $event)"
        />
      </ZFormItem>

      <div class="grid gap-3">
        <ZFormItem description="上游信息透传">
          <ZSwitch :model-value="form.quotaSharingEnabled" label="允许查询信息" :disabled="saving" @update:model-value="updateForm('quotaSharingEnabled', $event)" />
        </ZFormItem>
        <ZFormItem v-if="form.quotaSharingEnabled" class="rounded-cp bg-cp-fill-quaternary p-4">
          <div class="grid grid-cols-2 items-center gap-x-6 gap-y-3">
            <ZSwitch :model-value="true" label="传递账号额度" aria-readonly="true" class="cursor-default!" @click.prevent />
            <ZSwitch :model-value="form.quotaSharingIncludeNames" label="传递账号名称" :disabled="saving" @update:model-value="updateForm('quotaSharingIncludeNames', $event)" />
            <ZSwitch :model-value="form.quotaSharingIncludeModels" label="传递模型列表" :disabled="saving" @update:model-value="updateForm('quotaSharingIncludeModels', $event)" />
          </div>
        </ZFormItem>
      </div>

      <ZFormItem label="上游身份">
        <ProviderRequestProfilesEditor
          :model-value="form.providerRequestProfileOverrides" allow-inherit
          :active="open"
          :disabled="saving"
          @update:model-value="updateForm('providerRequestProfileOverrides', $event)"
        />
      </ZFormItem>

      <div class="grid gap-6 sm:grid-cols-2">
        <ZFormItem label="日限额">
          <ZInput
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
          </ZInput>
        </ZFormItem>
        <ZFormItem label="周限额">
          <ZInput
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
          </ZInput>
        </ZFormItem>
      </div>

      <div class="grid gap-6 sm:grid-cols-2">
        <ZFormItem label="最大并发">
          <ZInput
            :model-value="form.maxConcurrency" type="number"
            aria-label="最大并发"
            min="0"
            step="1"
            placeholder="不限制"
            :disabled="saving"
            @update:model-value="updateForm('maxConcurrency', $event)"
          />
        </ZFormItem>
        <ZFormItem label="每分钟请求数（RPM）">
          <ZInput
            :model-value="form.requestsPerMinute" type="number"
            aria-label="每分钟请求数（RPM）"
            min="0"
            step="1"
            placeholder="不限制"
            :disabled="saving"
            @update:model-value="updateForm('requestsPerMinute', $event)"
          />
        </ZFormItem>
      </div>
    </ZForm>

    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        取消
      </ZButton>
      <ZButton
        type="primary"
        :loading="saving"
        :disabled="!form.name.trim()"
        @click="emit('save')"
      >
        {{ editing ? '保存更改' : '创建' }}
      </ZButton>
    </template>
  </ZDialog>
</template>

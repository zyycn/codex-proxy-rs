<script setup lang="ts">
import type { AccountGroupFormValue } from '../composables/useAccountGroups'
import type { AccountGroup } from '@/api'
import { ZButton, ZColorPicker, ZDialog, ZForm, ZFormItem, ZInput, ZSegmented, ZTextarea } from '@codex-proxy/ui'

import { computed } from 'vue'
import { ACCOUNT_GROUP_COLOR_PRESETS } from '../constants'

const props = defineProps<{
  group: AccountGroup | null
  saving: boolean
}>()
const emit = defineEmits<{
  save: []
}>()
const open = defineModel<boolean>({ required: true })
const form = defineModel<AccountGroupFormValue>('form', { required: true })
const title = computed(() => props.group ? '编辑分组' : '创建分组')
const description = computed(() => props.group
  ? '修改分组信息和请求权限'
  : '创建后，可在账号管理中将账号加入这个分组')
</script>

<template>
  <ZDialog
    v-model="open"
    :title="title"
    :description="description"
    width="36rem"
    :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving"
  >
    <ZForm class="grid gap-5">
      <ZFormItem label="分组名称" required>
        <ZInput
          v-model="form.name"
          aria-label="分组名称"
          placeholder="例如：生产账号"
          :disabled="saving"
        />
      </ZFormItem>
      <ZFormItem label="分组颜色" required>
        <ZColorPicker
          v-model="form.color"
          aria-label="选择分组颜色"
          :presets="ACCOUNT_GROUP_COLOR_PRESETS"
          :disabled="saving"
        />
      </ZFormItem>
      <div class="flex flex-wrap items-start gap-x-6 gap-y-3">
        <ZFormItem label="快速模式" class="min-w-0">
          <ZSegmented
            v-model="form.fastMode"
            class="w-64 max-w-full"
            aria-label="快速模式"
            :options="[
              { label: '默认', value: 'default' },
              { label: '开启', value: 'enabled' },
              { label: '关闭', value: 'disabled' },
            ]"
            :disabled="saving"
          />
        </ZFormItem>
        <ZFormItem label="允许生图" class="min-w-0">
          <ZSegmented
            :model-value="form.allowImageGeneration ? 'enabled' : 'disabled'"
            class="w-40 max-w-full"
            aria-label="允许生图"
            :options="[
              { label: '开启', value: 'enabled' },
              { label: '关闭', value: 'disabled' },
            ]"
            :disabled="saving"
            @update:model-value="form.allowImageGeneration = $event === 'enabled'"
          />
        </ZFormItem>
      </div>
      <ZFormItem label="描述（可选）">
        <ZTextarea
          v-model="form.description"
          aria-label="分组描述"
          :rows="4"
          placeholder="说明这个分组的用途..."
          :disabled="saving"
        />
      </ZFormItem>
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
        保存分组
      </ZButton>
    </template>
  </ZDialog>
</template>

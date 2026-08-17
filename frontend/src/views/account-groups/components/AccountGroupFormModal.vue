<script setup lang="ts">
import type { AccountGroupFormValue } from '../composables/useAccountGroups'
import type { AccountGroup } from '@/api'
import { computed } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseColorPicker from '@/components/base/BaseColorPicker/index.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
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
  ? '修改分组名称和用途说明。'
  : '创建后，可在账号管理中将账号加入这个分组。')
</script>

<template>
  <BaseModal
    v-model="open"
    :title="title"
    :description="description"
    size="md"
    :dismissible="!saving"
  >
    <BaseForm class="grid gap-5">
      <BaseFormItem label="分组名称" required>
        <BaseInput
          v-model="form.name"
          aria-label="分组名称"
          placeholder="例如：生产账号"
          :disabled="saving"
        />
      </BaseFormItem>
      <BaseFormItem label="分组颜色" required>
        <BaseColorPicker
          v-model="form.color"
          label="选择分组颜色"
          :presets="ACCOUNT_GROUP_COLOR_PRESETS"
          :disabled="saving"
        />
      </BaseFormItem>
      <BaseFormItem label="描述（可选）">
        <BaseTextarea
          v-model="form.description"
          aria-label="分组描述"
          :rows="4"
          placeholder="说明这个分组的用途..."
          :disabled="saving"
        />
      </BaseFormItem>
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
        保存分组
      </BaseButton>
    </template>
  </BaseModal>
</template>

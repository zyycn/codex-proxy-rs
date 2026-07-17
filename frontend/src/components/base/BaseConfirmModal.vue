<script setup lang="ts">
import BaseButton from './BaseButton.vue'
import BaseModal from './BaseModal.vue'

type ConfirmVariant = 'info' | 'warning' | 'danger' | 'success'
type ButtonVariant = 'primary' | 'success' | 'warning' | 'danger'

const props = withDefaults(
  defineProps<{
    title: string
    description?: string
    variant?: ConfirmVariant
    confirmText?: string
    cancelText?: string
    loading?: boolean
    confirmDisabled?: boolean
    width?: string | number
  }>(),
  {
    variant: 'warning',
    confirmText: '确认',
    cancelText: '取消',
    loading: false,
    confirmDisabled: false,
    width: '480px',
  },
)

const emit = defineEmits<{
  confirm: []
  cancel: []
}>()
const open = defineModel<boolean>({ default: false })
const confirmVariantMap: Record<ConfirmVariant, ButtonVariant> = {
  info: 'primary',
  warning: 'warning',
  danger: 'danger',
  success: 'success',
}

function handleCancel() {
  if (props.loading)
    return
  open.value = false
  emit('cancel')
}

function handleConfirm() {
  if (props.loading || props.confirmDisabled)
    return
  emit('confirm')
}
</script>

<template>
  <BaseModal
    v-model="open"
    :title="title"
    :description="description"
    :variant="variant"
    :width="width"
    :close-disabled="loading"
  >
    <div
      v-if="$slots.default"
      class="text-[14px] leading-[1.55] font-[620] text-(--cp-text-secondary)"
    >
      <slot />
    </div>

    <template #footer>
      <BaseButton variant="default" :disabled="loading" @click="handleCancel">
        {{ cancelText }}
      </BaseButton>
      <BaseButton
        :variant="confirmVariantMap[variant]"
        :loading="loading"
        :disabled="confirmDisabled"
        @click="handleConfirm"
      >
        {{ confirmText }}
      </BaseButton>
    </template>
  </BaseModal>
</template>

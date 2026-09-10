<script setup lang="ts">
import type { ApiKey, UnresolvedClientCharge } from '@/api'
import { computed, ref, watch } from 'vue'
import { getUnresolvedClientCharges, reconcileClientCharge } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'

const props = defineProps<{ apiKey: ApiKey | null }>()
const emit = defineEmits<{ reconciled: [] }>()
const open = defineModel<boolean>({ required: true })
const events = ref<UnresolvedClientCharge[]>([])
const requestId = ref('')
const amount = ref('')
const reason = ref('')
const loading = ref(false)
const saving = ref(false)
const error = ref('')
let generation = 0
const options = computed(() => events.value.map(event => ({ value: event.requestId, label: event.requestId })))
const selected = computed(() => events.value.find(event => event.requestId === requestId.value))
const valid = computed(() => /^\d{1,10}(?:\.\d{1,10})?$/.test(amount.value.trim()) && reason.value.trim().length > 0 && reason.value.length <= 1024)

async function load() {
  const id = props.apiKey?.id
  const version = ++generation
  if (!id || !open.value)
    return
  loading.value = true
  error.value = ''
  events.value = []
  try {
    const result = await getUnresolvedClientCharges(id)
    if (version !== generation)
      return
    events.value = result.items
    requestId.value = result.items[0]?.requestId ?? ''
  }
  catch (failure) {
    if (version === generation)
      error.value = errorMessage(failure, '待核账请求读取失败')
  }
  finally {
    if (version === generation)
      loading.value = false
  }
}

async function save() {
  const id = props.apiKey?.id
  if (!id || !requestId.value || !valid.value || saving.value)
    return
  saving.value = true
  try {
    await reconcileClientCharge({ id, requestId: requestId.value, amountUsd: amount.value.trim(), reason: reason.value.trim() })
    toast.success('费用已核账')
    emit('reconciled')
    await load()
    if (events.value.length === 0 && !error.value)
      open.value = false
  }
  catch (failure) {
    toast.error(errorMessage(failure, '核账失败'))
  }
  finally {
    saving.value = false
  }
}

watch([open, () => props.apiKey?.id], () => {
  amount.value = ''
  reason.value = ''
  void load()
})
watch(requestId, () => {
  amount.value = ''
  reason.value = ''
})
</script>

<template>
  <BaseModal v-model="open" title="费用核账" :description="apiKey?.name" size="md" :dismissible="!saving">
    <p v-if="loading" class="text-cp-text-tertiary">
      读取中…
    </p>
    <div v-else-if="error" class="grid gap-3">
      <p class="text-cp-error">
        {{ error }}
      </p>
      <BaseButton variant="secondary" @click="load">
        重试
      </BaseButton>
    </div>
    <div v-else-if="events.length" class="grid gap-4">
      <BaseFormItem label="待核账请求">
        <BaseSelect v-model="requestId" :options="options" :disabled="saving" />
      </BaseFormItem>
      <p v-if="selected" class="m-0 text-xs text-cp-text-tertiary">
        {{ new Date(selected.startedAt).toLocaleString('zh-CN') }}
      </p>
      <BaseFormItem label="确认费用（USD）" required>
        <BaseInput v-model="amount" aria-label="确认费用（USD）" type="number" min="0" step="any" :disabled="saving" />
      </BaseFormItem>
      <BaseFormItem label="核账原因" required>
        <BaseInput v-model="reason" aria-label="核账原因" :maxlength="1024" :disabled="saving" />
      </BaseFormItem>
    </div>
    <p v-else class="text-cp-text-tertiary">
      暂无待核账请求
    </p>
    <template #footer>
      <BaseButton variant="ghost" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton variant="primary" :loading="saving" :disabled="!valid || !requestId || loading || Boolean(error)" @click="save">
        确认核账
      </BaseButton>
    </template>
  </BaseModal>
</template>

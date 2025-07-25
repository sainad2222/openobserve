<template>
    <q-page class="q-pa-none" style="min-height: inherit" 
    :class="store.state.theme === 'dark' ? 'dark-theme-list' : 'light-theme-list'"
    >
        <q-table
          v-if="!showImportRegexPatternDialog"
          data-test="regex-pattern-list-table"
          ref="regexPatternListTableRef"
          :rows="regexPatterns"
          :columns="columns"
          row-key="id"
          style="width: 100%"
          :pagination="pagination"
          :filter="filterQuery"
          :filter-method="filterData"
          class="regex-pattern-list-table"
          :class="store.state.theme === 'dark' ? 'dark-theme-regex-pattern-list' : 'light-theme-regex-pattern-list'"
        >
        <template #no-data>
          <NoRegexPatterns v-if="!listLoading && filterQuery == ''" @create-new-regex-pattern="createRegexPattern" @import-regex-pattern="importRegexPattern" />
          <div v-else-if="!listLoading && filterQuery != ''" class="full-width column flex-center q-mt-xs" style="font-size: 1.5rem">
            <NoData />
          </div>
          <div v-else class="full-width column flex-center q-mt-xs" style="font-size: 1.5rem">
            <q-spinner-hourglass size="50px" color="primary" style="margin-top: 20vh" />

          </div>
        </template> 
        <template #top="scope">
          <div class="q-table__title" data-test="regex-pattern-list-title">
            {{ t("regex_patterns.header") }}
          </div>
          <q-input
            data-test="regex-pattern-list-search-input"
            v-model="filterQuery"
            borderless
            filled
            dense
            class="q-ml-auto no-border"
            :placeholder="t('regex_patterns.search')"
          >
            <template #prepend>
              <q-icon name="search" class="cursor-pointer" />
            </template>
          </q-input>
          <q-btn
            class="q-ml-md text-bold"
            padding="sm lg"
            outline
            no-caps
            :label="t(`regex_patterns.import`)"
            @click="importRegexPattern"
            data-test="regex-pattern-list-import"
          />
          <q-btn
            data-test="regex-pattern-list-add-pattern-btn"
            class="q-ml-md text-bold no-border"
            padding="sm lg"
            color="secondary"
            no-caps
            :label="t(`regex_patterns.create_pattern`)"
            @click="createRegexPattern"
          />
          <QTablePagination
            data-test="regex-pattern-list-pagination"
            :scope="scope"
            :pageTitle="t('regex_patterns.header')"
            :position="'top'"
            :resultTotal="resultTotal"
            :perPageOptions="perPageOptions"
            @update:changeRecordPerPage="changePagination"
          />
        </template>
        <template v-slot:header="props">
          <!-- render the header of the columns -->
          <q-th
            v-for="col in props.cols"
            :key="col.name"
            :props="props"
            :class="col.classes"
            :style="col.style"
          >
            {{ col.label }}
          </q-th>
        </template>
        <template v-slot:body="props">
          <q-tr :props="props">

          <!-- render the body of the columns -->
          <q-td v-for="col in columns" :key="col.name " :props="props">
            <template v-if="col.name  !== 'actions'">
              {{ props.row[col.field] }}
            </template>
            <template v-else>
              <q-btn
              icon="download"
              title="Export Regex Pattern"
              class="q-ml-xs"
              padding="sm"
              unelevated
              size="sm"
              round
              flat
              @click.stop="exportRegexPattern(props.row)"
              :data-test="`regex-pattern-list-${props.row.id}-export-regex-pattern`"
            ></q-btn>
              <q-btn
              :data-test="`regex-pattern-list-${props.row.id}-update-regex-pattern`"
              icon="edit"
              class="q-ml-xs"
              padding="sm"
              unelevated
              size="sm"
              round
              flat
              :title="t('regex_patterns.edit')"
              @click.stop="editRegexPattern(props.row)"
            ></q-btn>
            <q-btn
              :data-test="`regex-pattern-list-${props.row.id}-delete-regex-pattern`"
              :icon="outlinedDelete"
              class="q-ml-xs"
              padding="sm"
              unelevated
              size="sm"
              round
              flat
              :title="t('regex_patterns.delete')"
              @click.stop="confirmDeleteRegexPattern(props.row)"
            ></q-btn>
            </template>
          </q-td>
                      
        </q-tr>
        </template>
        <template #bottom="scope">
          <QTablePagination
            :scope="scope"
            :position="'bottom'"
            :resultTotal="resultTotal"
            :perPageOptions="perPageOptions"
            @update:changeRecordPerPage="changePagination"
          />
        </template>
        </q-table>
        <ImportRegexPattern 
          v-else-if="showImportRegexPatternDialog" 
          @cancel:hideform="showImportRegexPatternDialog = false" 
          @update:list="getRegexPatterns" 
          :regex-patterns="regexPatterns.map(pattern => pattern.name)" 
        />
        <ConfirmDialog
          v-model="deleteDialog.show"
          :title="deleteDialog.title"
          :message="deleteDialog.message"
          @update:ok="deleteRegexPattern"
          @update:cancel="deleteDialog.show = false"
        />
        <q-dialog v-model="showAddRegexPatternDialog.show" position="right" full-height maximized>
          <AddRegexPattern :data="showAddRegexPatternDialog.data" :is-edit="showAddRegexPatternDialog.isEdit" @update:list="getRegexPatterns" @close="closeAddRegexPatternDialog" />
        </q-dialog>
    </q-page>
  </template>

<script lang="ts">
    import { ref, onBeforeMount, onActivated, watch, defineComponent, onMounted } from "vue"; 
    import type { Ref } from "vue";
    import { useI18n } from "vue-i18n";
    import { useQuasar, type QTableProps } from "quasar";
    import { convertUnixToQuasarFormat, getImageURL } from "@/utils/zincutils";
    import ConfirmDialog from "../ConfirmDialog.vue";
    import { useRouter } from "vue-router";
    import QTablePagination from "@/components/shared/grid/Pagination.vue";
    import { useStore } from "vuex";
    import NoRegexPatterns from "./NoRegexPatterns.vue";
    import regexPatternsService from "@/services/regex_pattern";
    import { outlinedDelete } from "@quasar/extras/material-icons-outlined";
    import AddRegexPattern from "./AddRegexPattern.vue";
    import ImportRegexPattern from "./ImportRegexPattern.vue";
    import config from "@/aws-exports";
    import NoData from "@/components/shared/grid/NoData.vue";

    export default defineComponent({
        name: "RegexPatternList",
        components: {
            QTablePagination,
            NoRegexPatterns,
            ConfirmDialog,
            AddRegexPattern,
            ImportRegexPattern,
            NoData
        },
    setup() {

    const regexPatternListTableRef = ref(null);
    const pagination: any = ref({
        rowsPerPage: 20,
      });
    const filterQuery = ref("");
    const { t } = useI18n();
    const store = useStore();
    const router = useRouter();


    const columns = ref<any>([
        {
          name: "#",
          label: "#",
          field: "#",
          align: "left",
          style: "width: 67px",
        },
        {
          name: "name",
          field: "name",
          label: t("regex_patterns.name"),
          align: "left",
          sortable: true,
        },
        {
          name: "pattern",
          field: "pattern",
          label: t("regex_patterns.pattern"),
          align: "left",
        },
        {
          name: "created_at",
          field: "created_at",
          label: t("regex_patterns.created_at"),
          align: "left",
          style: "width: 150px",
        },
        {
          name: "updated_at",
          field: "updated_at",
          label: t("regex_patterns.updated_at"),
          align: "left",
          sortable: true,
          style: "width: 150px",
        },
        {
          name: "actions",
          field: "actions",
          label: t("regex_patterns.actions"),
          align: "left",
          classes: "actions-column",
        }
      ]);
    const deleteDialog = ref({
      show: false,
      title: "Delete Regex Pattern",
      message: "Are you sure you want to delete this regex pattern?",
      data: "" as any,
    });

    const regexPatterns = ref([]);

    const resultTotal = ref(0);

    const perPageOptions = ref([10, 20, 50, 100]);

    const $q = useQuasar();

    const listLoading = ref(false);
    const selectedPerPage = ref<number>(20);

    const showImportRegexPatternDialog = ref(false);


    const showAddRegexPatternDialog = ref({
      show: false,
      data: {} as any,
      isEdit: false,
    });

    onMounted(async () => {
      //if the regex patterns are not in the store, then we need to fetch them from the api
      //else we can use the regex patterns from the store
      //this is done to avoid multiple api calls while switching between the regex pattern list and other pages
      if(store.state.organizationData.regexPatterns.length == 0){
        await getRegexPatterns();
      }
      else{
        regexPatterns.value = store.state.organizationData.regexPatterns;
        resultTotal.value = regexPatterns.value.length;
      }
      // we need to open the add regex pattern dialog if the user has been redirected from the logs page
      if(router.currentRoute.value.query.from == 'logs' && config.isEnterprise == 'true'){
        createRegexPattern();
      }
      //this is kept so that when user tries to refresh the page while in import regex pattern dialog, the dialog is not closed
      if(router.currentRoute.value.query.action == 'import' && config.isEnterprise == 'true'){
        importRegexPattern();
      }
    });

    const changePagination = (val: { label: string; value: any }) => { 
      //this is done sometimes we are getting the value as an object and sometimes as a number
      //so we need to handle both cases
      let rowsPerPage = val.value ? val.value : val;
      selectedPerPage.value = rowsPerPage;
      pagination.value.rowsPerPage = rowsPerPage;
      regexPatternListTableRef.value?.setPagination(pagination.value);
    };

    watch(filterQuery, (newVal) => {
      if(newVal == ""){
        resultTotal.value = regexPatterns.value.length;
      }
    })
    //this is used to filter the regex pattern based on the name using the search input
    const filterData = (rows: any, terms: any) => {
        var filtered = [];
        terms = terms.toLowerCase();
        for (var i = 0; i < rows.length; i++) {
          if (rows[i]["name"].toLowerCase().includes(terms)) {
            filtered.push(rows[i]);
          }
        }
        resultTotal.value = filtered.length;
        return filtered;
    };


    const createRegexPattern = () => {
      showAddRegexPatternDialog.value.show = true;
      showAddRegexPatternDialog.value.isEdit = false;
      showAddRegexPatternDialog.value.data = {};
    }
    //call this function only on mounted or any changes in the list like updating the existing regex pattern or deleting the existing regex pattern
    //we are storing the regex patterns in the store to avoid multiple api calls and it is stored in the organizationData.regexPatterns
    //it can be reused because after converting all the required data to quasar format, it can be used in the table
    const getRegexPatterns = async () => {
      listLoading.value = true;
      try{
        const response = await regexPatternsService.list(store.state.selectedOrganization.identifier);
        let counter = 1;
        regexPatterns.value = response.data.patterns.map((pattern: any) => ({
          ...pattern,
          "#": counter <= 9 ? `0${counter++}` : counter++,
          created_at: convertUnixToQuasarFormat(pattern.created_at),
          updated_at: convertUnixToQuasarFormat(pattern.updated_at),
        }));
        store.dispatch("setRegexPatterns", regexPatterns.value);
        resultTotal.value = regexPatterns.value.length;
      } catch (error) {
        $q.notify({
          message: error.data.message || "Error fetching regex patterns",
          color: "negative",
          icon: "error",
        });
      }
      finally{
        listLoading.value = false;
      }
    }

    const editRegexPattern = (row: any) => {
      showAddRegexPatternDialog.value.show = true;
      showAddRegexPatternDialog.value.isEdit = true;
      showAddRegexPatternDialog.value.data = row;
    }

    const confirmDeleteRegexPattern = (row: any) => {
      deleteDialog.value.show = true;
      deleteDialog.value.data = row.id;
    }

    const deleteRegexPattern = async () => {
      try{
        await regexPatternsService.delete(store.state.selectedOrganization.identifier, deleteDialog.value.data);
        getRegexPatterns();
        $q.notify({
          message: `Regex pattern deleted successfully.`,
          color: "positive",
          timeout: 1500,
        });
      } catch (error) {
        $q.notify({
          message: error?.data?.message || error?.response?.data?.message || "Error deleting regex pattern",
          color: "negative",
          timeout: 1500,
        });
      }
    }

    const importRegexPattern = () => {
      showImportRegexPatternDialog.value = true;
      router.push({
        path: '/settings/regex_patterns',
        query: {
          org_identifier: store.state.selectedOrganization.identifier,
          action: 'import'
        },
      })
    }

    const exportRegexPattern = (row: any) => {
      let url = "";
      try{
      const regexPatternToBeExported = {
        name: row.name,
        pattern: row.pattern,
        description: row.description,
      }

      const regexPatternJson = JSON.stringify(regexPatternToBeExported, null, 2);
      const blob = new Blob([regexPatternJson], { type: "application/json" });
      url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = `${row.name || 'regex_pattern'}.json`;
      link.click();
      $q.notify({
        message: `Regex pattern exported successfully`,
        color: "positive",
        icon: "check",
      });
    } catch (error) {
      $q.notify({
        message: error.data.message || "Error exporting regex pattern",
        color: "negative",
        icon: "error",
      });
    }
    finally{
      //this is added for proper clean up of the blob url
      if(url){
        URL.revokeObjectURL(url);
      }
    }
  }
  const closeAddRegexPatternDialog = () => {
    //reset the values if any before closing the dialog
    //this will make sure that the values are not set in the ai chat input context when user clicks on the create regex pattern button again
    showAddRegexPatternDialog.value.show = false;
    store.state.organizationData.regexPatternPrompt = "";
    store.state.organizationData.regexPatternTestValue = "";
    router.push({
      path: '/settings/regex_patterns',
      query:{
        org_identifier: store.state.selectedOrganization.identifier,
      }
    })
  }

    return {
        t,
        store,
        router,
        regexPatternListTableRef,
        pagination,
        filterQuery,
        columns,
        regexPatterns,
        filterData,
        resultTotal,
        perPageOptions,
        changePagination,
        createRegexPattern,
        listLoading,
        outlinedDelete,
        editRegexPattern,
        deleteRegexPattern,
        deleteDialog,
        confirmDeleteRegexPattern,
        showAddRegexPatternDialog,
        getRegexPatterns,
        selectedPerPage,
        showImportRegexPatternDialog,
        importRegexPattern,
        exportRegexPattern,
        closeAddRegexPatternDialog
    }
    }
})

</script>

<style lang="scss">
.regex-pattern-list-table{
  th,td {
    padding: 0px 16px !important;
    height: 36px !important;
  };
  th:last-child,
  td:last-child {
    position: sticky;
    right: 0;
    z-index: 1;
  }
}


.dark-theme-regex-pattern-list{
  th:last-child.actions-column,
  td:last-child.actions-column {
    background: var(--q-dark);
    box-shadow: -4px 0px 4px 0 rgba(144, 144, 144, 0.1);
    width: 120px;
  }
}

.light-theme-regex-pattern-list{
  th:last-child.actions-column,
  td:last-child.actions-column {
    background: #ffffff;
    width: 120px;
    box-shadow: -4px 0px 4px 0 rgba(0, 0, 0, 0.1);
  }
}
</style>
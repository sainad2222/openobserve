// Copyright 2023 OpenObserve Inc.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

import { describe, expect, it, beforeEach } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { installQuasar } from "@/test/unit/helpers/install-quasar-plugin";
import { createStore } from "vuex";
import FluentBit from "@/components/ingestion/logs/FluentBit.vue";

installQuasar();
const store = createStore({
  state: {
    organizationPasscode: 11,
    API_ENDPOINT: "http://localhost:808",
  },
});
describe.skip("FluentBit", async () => {
  let wrapper: any;
  beforeEach(() => {
    wrapper = mount(FluentBit, {
      props: {
        currOrgIdentifier: "zinc_next",
        currUserEmail: "tulsiraval2828@gmail.com",
        orgAPIKey: 'L"4R{8f~56e72`0319V',
      },
      global: {
        provide: {
          store: store,
        },
      },
    });
  });
  it("should mount FluentBit", async () => {
    expect(wrapper.vm.currOrgIdentifier).not.toBe("");
    expect(wrapper.vm.currUserEmail).not.toBe("");
    expect(wrapper.vm.orgAPIKey).not.toBe("");
  });

  it("should render title", () => {
    const title = wrapper.find('[data-test="fluent-bit-title-text"]');
    expect(title.text()).toBe("Fluent Bit");
  });

  it("should render content", async () => {
    await flushPromises();
    const content = wrapper.find('[data-test="fluent-bit-content-text"]');
    expect(content.text()).matchSnapshot();
  });

  it("should render copy icon", () => {
    const btn = wrapper.find('[data-test="fluent-bit-copy-btn"]');
    expect(btn.exists()).toBeTruthy();
  });
});

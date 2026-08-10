// MongoDB sample: document-oriented ERP data
// Simulates what the MongoDB connector's introspect_all() would discover by sampling documents.

// Collection: suppliers
db.createCollection("suppliers");
db.suppliers.insertMany([
  {
    name: "Om Building Materials",
    gstin: "10CQAPS3890L1ZM",
    type: "supplier",
    address: {
      street: "Shop No. 12, Gandhi Nagar",
      city: "Patna",
      state: "Bihar",
      pincode: "800001"
    },
    bank_account: {
      bank: "HDFC Bank",
      branch: "Gandhi Nagar, Patna",
      account_no: "50200012345678",
      ifsc: "HDFC0001234",
      upi: "ombuilding@hdfcbank"
    },
    contact: {
      name: "Ramesh Kumar Agarwal",
      phone: "0612-2345678",
      email: "sales@ombuilding.com"
    }
  }
]);

// Collection: purchase_orders
db.createCollection("purchase_orders");
db.purchase_orders.insertMany([
  {
    order_number: "PO-2024-001",
    supplier_name: "Om Building Materials",
    order_date: new Date("2024-07-15"),
    status: "delivered",
    items: [
      { product: "Grey Cement", hsn: "2523291", qty: 220, unit: "Bags", rate: 590.00, gst_rate: 28 },
      { product: "Fe 500 TMT Bar", hsn: "7214200", qty: 10, unit: "MT", rate: 58500.00, gst_rate: 18 }
    ],
    taxable_amount: 714800.00,
    igst_amount: 141644.00,
    total_amount: 856444.00
  }
]);

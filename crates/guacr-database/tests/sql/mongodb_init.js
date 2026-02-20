// MongoDB initialization script for integration tests

// Switch to testdb (creates if not exists)
db = db.getSiblingDB('testdb');

// Create users collection with sample data
db.users.insertMany([
    {
        username: "alice",
        email: "alice@example.com",
        createdAt: new Date(),
        isActive: true,
        profile: {
            firstName: "Alice",
            lastName: "Smith",
            age: 28
        }
    },
    {
        username: "bob",
        email: "bob@example.com",
        createdAt: new Date(),
        isActive: true,
        profile: {
            firstName: "Bob",
            lastName: "Johnson",
            age: 35
        }
    },
    {
        username: "charlie",
        email: "charlie@example.com",
        createdAt: new Date(),
        isActive: false,
        profile: {
            firstName: "Charlie",
            lastName: "Brown",
            age: 42
        }
    }
]);

// Create products collection
db.products.insertMany([
    {
        name: "Widget A",
        price: 19.99,
        quantity: 100,
        tags: ["electronics", "popular"],
        metadata: {
            color: "red",
            size: "medium"
        }
    },
    {
        name: "Widget B",
        price: 29.99,
        quantity: 50,
        tags: ["electronics"],
        metadata: {
            color: "blue",
            size: "large"
        }
    },
    {
        name: "Gadget X",
        price: 99.99,
        quantity: 25,
        tags: ["premium", "new"],
        metadata: {
            warranty: "2 years"
        }
    }
]);

// Create indexes
db.users.createIndex({ username: 1 }, { unique: true });
db.users.createIndex({ email: 1 });
db.products.createIndex({ name: 1 });

print("MongoDB initialization complete");

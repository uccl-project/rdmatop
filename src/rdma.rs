//! RDMA netlink constants from `linux/rdma_netlink.h`.

#![allow(dead_code)]

pub const NETLINK_RDMA: i32 = 20;

// Clients
pub const RDMA_NL_NLDEV: u32 = 5;

pub const fn rdma_nl_get_type(client: u32, op: u32) -> u16 {
    ((client << 10) + op) as u16
}

// Commands
pub const RDMA_NLDEV_CMD_GET: u32 = 1;
pub const RDMA_NLDEV_CMD_PORT_GET: u32 = 5;
pub const RDMA_NLDEV_CMD_RES_QP_GET: u32 = 10;
pub const RDMA_NLDEV_CMD_STAT_GET: u32 = 17;

// Attributes
pub const RDMA_NLDEV_ATTR_DEV_INDEX: u16 = 1;
pub const RDMA_NLDEV_ATTR_DEV_NAME: u16 = 2;
pub const RDMA_NLDEV_ATTR_PORT_INDEX: u16 = 3;
pub const RDMA_NLDEV_ATTR_PORT_STATE: u16 = 16;
pub const RDMA_NLDEV_ATTR_RES_QP: u16 = 19;
pub const RDMA_NLDEV_ATTR_RES_QP_ENTRY: u16 = 20;
pub const RDMA_NLDEV_ATTR_RES_LQPN: u16 = 21;
pub const RDMA_NLDEV_ATTR_RES_TYPE: u16 = 26;
pub const RDMA_NLDEV_ATTR_RES_STATE: u16 = 27;
pub const RDMA_NLDEV_ATTR_RES_PID: u16 = 28;
pub const RDMA_NLDEV_ATTR_RES_KERN_NAME: u16 = 29;
pub const RDMA_NLDEV_ATTR_STAT_HWCOUNTERS: u16 = 80;
pub const RDMA_NLDEV_ATTR_STAT_HWCOUNTER_ENTRY: u16 = 81;
pub const RDMA_NLDEV_ATTR_STAT_HWCOUNTER_ENTRY_NAME: u16 = 82;
pub const RDMA_NLDEV_ATTR_STAT_HWCOUNTER_ENTRY_VALUE: u16 = 83;

// Netlink
pub const NLM_F_REQUEST: u16 = 1;
pub const NLM_F_ACK: u16 = 4;
pub const NLM_F_DUMP: u16 = 0x300;
pub const NLMSG_DONE: u16 = 3;
pub const NLMSG_ERROR: u16 = 2;
pub const NLA_F_NESTED: u16 = 1 << 15;
